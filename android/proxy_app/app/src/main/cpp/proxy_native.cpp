// ============================================================================
// PhantomCore — Android Proxy Native Bridge (C++ / JNI)
// Reads eBPF ring buffer, serializes offload requests, sends UDP to PC daemon
// ============================================================================

#include <jni.h>
#include <android/log.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <unistd.h>
#include <pthread.h>
#include <string.h>
#include <stdlib.h>
#include <errno.h>
#include <time.h>
#include <atomic>
#include <sys/epoll.h>

#define TAG "PhantomProxy"
#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, TAG, __VA_ARGS__)
#define LOGW(...) __android_log_print(ANDROID_LOG_WARN, TAG, __VA_ARGS__)
#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, TAG, __VA_ARGS__)

// Protocol constants
#define HEADER_SIZE         16
#define MAX_UDP_PAYLOAD     65000
#define DAEMON_PORT         42069
#define FEC_BLOCK_SIZE      8
#define FEC_PARITY_COUNT    2
#define NACK_INTERVAL_MS    5

// ---- Offload Request (matches eBPF struct) ----
struct offload_req {
    uint32_t pid;
    uint32_t tgid;
    uint32_t syscall_id;
    uint64_t arg1;
    uint64_t arg2;
    uint64_t arg3;
    uint64_t arg4;
    uint64_t arg5;
    uint64_t arg6;
    uint64_t timestamp;
};

// ---- Protocol Packet ----
struct __attribute__((packed)) proto_header {
    uint32_t session_id;
    uint32_t seq;
    uint32_t func_id;
    uint16_t flags;
    uint16_t payload_len;
};

// ---- FEC Encoder ----
typedef struct {
    uint8_t* packets[FEC_BLOCK_SIZE];
    int      packet_lens[FEC_BLOCK_SIZE];
    int      count;
    int      max_len;
} fec_encoder_t;

static void fec_init(fec_encoder_t* enc) {
    memset(enc, 0, sizeof(fec_encoder_t));
}

static void xor_into(uint8_t* dst, const uint8_t* src, int dst_len, int src_len) {
    int min_len = dst_len < src_len ? dst_len : src_len;
    for (int i = 0; i < min_len; i++) {
        dst[i] ^= src[i];
    }
}

// Returns 1 if parity packets were generated, 0 otherwise
static int fec_add_packet(fec_encoder_t* enc, const uint8_t* data, int len,
                          uint8_t* parity1, int* p1_len, uint8_t* parity2, int* p2_len) {
    int idx = enc->count;
    enc->packets[idx] = (uint8_t*)malloc(len);
    memcpy(enc->packets[idx], data, len);
    enc->packet_lens[idx] = len;
    if (len > enc->max_len) enc->max_len = len;
    enc->count++;

    if (enc->count == FEC_BLOCK_SIZE) {
        // Generate parity
        *p1_len = enc->max_len;
        *p2_len = enc->max_len;
        memset(parity1, 0, enc->max_len);
        memset(parity2, 0, enc->max_len);

        for (int i = 0; i < 4; i++) {
            xor_into(parity1, enc->packets[i], *p1_len, enc->packet_lens[i]);
        }
        for (int i = 4; i < 8; i++) {
            xor_into(parity2, enc->packets[i], *p2_len, enc->packet_lens[i]);
        }

        // Cleanup
        for (int i = 0; i < FEC_BLOCK_SIZE; i++) {
            free(enc->packets[i]);
            enc->packets[i] = NULL;
        }
        enc->count = 0;
        enc->max_len = 0;
        return 1;
    }
    return 0;
}

// ---- Proxy State ----
typedef struct {
    std::atomic<int>      running;
    int             udp_sock;
    struct sockaddr_in daemon_addr;
    pthread_t       proxy_thread;
    pthread_t       recv_thread;

    // Stats
    std::atomic<long>     packets_sent;
    std::atomic<long>     packets_received;
    std::atomic<long>     total_latency_us;

    // Sequence counter
    std::atomic<uint32_t>     seq_counter;

    // eBPF ring buffer fd
    int             ringbuf_fd;

    // FEC
    fec_encoder_t   fec;
} proxy_state_t;

static proxy_state_t g_state = {0};

// ---- Serialize offload request to protocol format ----
static int serialize_request(const struct offload_req* req, uint32_t seq,
                             uint8_t* buf, int buf_size) {
    if (buf_size < HEADER_SIZE + 6 * 8) return -1;

    struct proto_header* hdr = (struct proto_header*)buf;
    hdr->session_id = req->pid;
    hdr->seq = seq;
    hdr->func_id = req->syscall_id;
    hdr->flags = 0;

    // Payload: 6 arguments as uint64_t
    uint8_t* payload = buf + HEADER_SIZE;
    int offset = 0;

    memcpy(payload + offset, &req->arg1, 8); offset += 8;
    memcpy(payload + offset, &req->arg2, 8); offset += 8;
    memcpy(payload + offset, &req->arg3, 8); offset += 8;
    memcpy(payload + offset, &req->arg4, 8); offset += 8;
    memcpy(payload + offset, &req->arg5, 8); offset += 8;
    memcpy(payload + offset, &req->arg6, 8); offset += 8;

    hdr->payload_len = offset;
    return HEADER_SIZE + offset;
}

// ---- Receive thread: reads responses from PC daemon ----
static void* recv_thread_func(void* arg) {
    (void)arg;
    uint8_t buf[MAX_UDP_PAYLOAD];
    LOGI("Receive thread started");

    while (g_state.running.load()) {
        struct sockaddr_in from_addr;
        socklen_t from_len = sizeof(from_addr);

        ssize_t n = recvfrom(g_state.udp_sock, buf, sizeof(buf), 0,
                            (struct sockaddr*)&from_addr, &from_len);
        if (n < 0) {
            if (errno == EAGAIN || errno == EWOULDBLOCK) continue;
            LOGE("recvfrom error: %s", strerror(errno));
            break;
        }

        if (n < HEADER_SIZE) continue;

        struct proto_header* hdr = (struct proto_header*)buf;
        g_state.packets_received.fetch_add(1);

        LOGI("Response seq=%u, payload=%u bytes", hdr->seq, hdr->payload_len);

        // In a full implementation, we'd write the result back to the kernel
        // via ioctl on a phantom device or shared memory region.
        // For the demo, we just log and track latency.
    }

    LOGI("Receive thread exiting");
    return NULL;
}

// ---- Proxy thread: reads eBPF ring buffer and sends to daemon ----
static void* proxy_thread_func(void* arg) {
    (void)arg;
    LOGI("Proxy thread started");

    uint8_t send_buf[MAX_UDP_PAYLOAD];
    uint8_t parity1[MAX_UDP_PAYLOAD];
    uint8_t parity2[MAX_UDP_PAYLOAD];
    int p1_len, p2_len;

    // Try to open eBPF ring buffer map
    // In production, this would be:
    //   g_state.ringbuf_fd = bpf_obj_get("/sys/fs/bpf/offload_ringbuf");
    // For the demo without root/eBPF, we simulate syscall interception

    LOGI("eBPF ring buffer fd=%d (simulated mode if -1)", g_state.ringbuf_fd);

    while (g_state.running.load()) {
        // Simulated: generate a synthetic offload request every 8ms
        // In production, this reads from the eBPF ring buffer via epoll
        struct timespec sleep_time = {0, 8000000}; // 8ms
        nanosleep(&sleep_time, NULL);

        if (!g_state.running.load()) break;

        // Create a simulated offload request
        struct offload_req req = {0};
        req.pid = getpid();
        req.tgid = getpid();
        req.syscall_id = 64; // write
        req.arg1 = 1;        // fd = stdout
        req.arg2 = 0x7000;   // buf pointer
        req.arg3 = 128;      // count

        struct timespec ts;
        clock_gettime(CLOCK_MONOTONIC, &ts);
        req.timestamp = ts.tv_sec * 1000000000ULL + ts.tv_nsec;

        uint32_t seq = g_state.seq_counter.fetch_add(1);
        int pkt_len = serialize_request(&req, seq, send_buf, sizeof(send_buf));

        if (pkt_len > 0) {
            sendto(g_state.udp_sock, send_buf, pkt_len, 0,
                   (struct sockaddr*)&g_state.daemon_addr,
                   sizeof(g_state.daemon_addr));
            g_state.packets_sent.fetch_add(1);

            // FEC: accumulate and send parity when block is complete
            if (fec_add_packet(&g_state.fec, send_buf, pkt_len,
                              parity1, &p1_len, parity2, &p2_len)) {
                sendto(g_state.udp_sock, parity1, p1_len, 0,
                       (struct sockaddr*)&g_state.daemon_addr,
                       sizeof(g_state.daemon_addr));
                sendto(g_state.udp_sock, parity2, p2_len, 0,
                       (struct sockaddr*)&g_state.daemon_addr,
                       sizeof(g_state.daemon_addr));
            }
        }
    }

    LOGI("Proxy thread exiting");
    return NULL;
}

// ---- JNI Exports ----

extern "C" {

JNIEXPORT jboolean JNICALL
Java_com_phantom_core_ProxyService_nativeStartProxy(JNIEnv* env, jobject thiz,
                                                     jstring daemon_ip, jint port) {
    if (g_state.running.load()) {
        LOGW("Proxy already running");
        return JNI_FALSE;
    }

    const char* ip = env->GetStringUTFChars(daemon_ip, NULL);
    LOGI("Starting proxy -> %s:%d", ip, port);

    // Create UDP socket
    g_state.udp_sock = socket(AF_INET, SOCK_DGRAM, 0);
    if (g_state.udp_sock < 0) {
        LOGE("Failed to create socket: %s", strerror(errno));
        env->ReleaseStringUTFChars(daemon_ip, ip);
        return JNI_FALSE;
    }

    // Set receive timeout (100ms) so recv thread can check running flag
    struct timeval tv = {0, 100000};
    setsockopt(g_state.udp_sock, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));

    // Configure daemon address
    memset(&g_state.daemon_addr, 0, sizeof(g_state.daemon_addr));
    g_state.daemon_addr.sin_family = AF_INET;
    g_state.daemon_addr.sin_port = htons(port);
    inet_pton(AF_INET, ip, &g_state.daemon_addr.sin_addr);

    env->ReleaseStringUTFChars(daemon_ip, ip);

    // Try to open eBPF map (will fail without root, which is fine for demo)
    g_state.ringbuf_fd = -1; // bpf_obj_get("/sys/fs/bpf/offload_ringbuf");

    // Reset state
    g_state.packets_sent.store(0);
    g_state.packets_received.store(0);
    g_state.total_latency_us.store(0);
    g_state.seq_counter.store(0);
    fec_init(&g_state.fec);

    // Start threads
    g_state.running.store(1);
    pthread_create(&g_state.proxy_thread, NULL, proxy_thread_func, NULL);
    pthread_create(&g_state.recv_thread, NULL, recv_thread_func, NULL);

    LOGI("Proxy started successfully");
    return JNI_TRUE;
}

JNIEXPORT void JNICALL
Java_com_phantom_core_ProxyService_nativeStopProxy(JNIEnv* env, jobject thiz) {
    LOGI("Stopping proxy...");
    g_state.running.store(0);

    pthread_join(g_state.proxy_thread, NULL);
    pthread_join(g_state.recv_thread, NULL);

    if (g_state.udp_sock >= 0) {
        close(g_state.udp_sock);
        g_state.udp_sock = -1;
    }

    LOGI("Proxy stopped");
}

JNIEXPORT jstring JNICALL
Java_com_phantom_core_ProxyService_nativeGetStats(JNIEnv* env, jobject thiz) {
    long sent = g_state.packets_sent.load();
    long received = g_state.packets_received.load();
    long latency = g_state.total_latency_us.load();
    double avg_lat = (received > 0) ? (double)latency / received : 0.0;

    char stats[256];
    snprintf(stats, sizeof(stats),
             "{\"sent\":%ld,\"received\":%ld,\"avg_latency_us\":%.1f,\"running\":%d}",
             sent, received, avg_lat, g_state.running.load());

    return env->NewStringUTF(stats);
}

} // extern "C"
