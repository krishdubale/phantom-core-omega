#include <linux/bpf.h>
#include <bpf/bpf_helpers.h>
#include <linux/ptrace.h>
#include <linux/sched.h>

char _license[] SEC("license") = "GPL";

// Phantom Core - eBPF Syscall Interceptor
// Intercepts __arm64_sys_write, __arm64_sys_ioctl, __arm64_sys_futex

struct offload_req {
    __u32 pid;
    __u32 tgid;
    __u32 syscall_id;
    __u64 arg1;
    __u64 arg2;
    __u64 arg3;
    __u64 arg4;
    __u64 arg5;
    __u64 arg6;
    __u64 timestamp;
};

struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, 256 * 1024);
} offload_ringbuf SEC(".maps");

struct {
    __uint(type, BPF_MAP_TYPE_HASH);
    __uint(max_entries, 1024);
    __type(key, __u32);   // PID
    __type(value, __u8);  // Offload Enabled Flag
} active_sessions SEC(".maps");

static __always_inline int process_syscall(struct pt_regs *ctx, __u32 syscall_id) {
    __u32 pid = bpf_get_current_pid_tgid() >> 32;
    
    // Check if this process is managed by Phantom Core
    __u8 *is_active = bpf_map_lookup_elem(&active_sessions, &pid);
    if (!is_active || *is_active == 0) {
        return 0; // Let it run locally
    }

    struct offload_req *req = bpf_ringbuf_reserve(&offload_ringbuf, sizeof(*req), 0);
    if (!req) {
        // Ringbuffer full, fallback to local execution
        return 0;
    }

    req->pid = pid;
    req->tgid = bpf_get_current_pid_tgid() & 0xFFFFFFFF;
    req->syscall_id = syscall_id;
    req->timestamp = bpf_ktime_get_ns();
    
    // Extract ARM64 syscall arguments from pt_regs
    req->arg1 = ctx->regs[0];
    req->arg2 = ctx->regs[1];
    req->arg3 = ctx->regs[2];
    req->arg4 = ctx->regs[3];
    req->arg5 = ctx->regs[4];
    req->arg6 = ctx->regs[5];

    bpf_ringbuf_submit(req, 0);

    // Override return value to -EAGAIN to block local execution and force user-space proxy handling
    bpf_override_return(ctx, -11); // -EAGAIN
    return 0;
}

SEC("kprobe/__arm64_sys_write")
int bpf_prog_write(struct pt_regs *ctx) {
    return process_syscall(ctx, 64); // __NR_write on arm64
}

SEC("kprobe/__arm64_sys_ioctl")
int bpf_prog_ioctl(struct pt_regs *ctx) {
    return process_syscall(ctx, 29); // __NR_ioctl on arm64
}

SEC("kprobe/__arm64_sys_futex")
int bpf_prog_futex(struct pt_regs *ctx) {
    return process_syscall(ctx, 98); // __NR_futex on arm64
}
