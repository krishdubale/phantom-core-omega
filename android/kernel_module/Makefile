# PhantomCore eBPF Kernel Module Makefile
# Compiles ebpf_program.c for loading on Android 12+ (kernel 5.10+)

CLANG ?= clang-14
LLC ?= llc-14
BPFTOOL ?= bpftool
KERNEL_HEADERS ?= /usr/src/linux-headers-$(shell uname -r)
ANDROID_KERNEL_HEADERS ?= $(ANDROID_BUILD_TOP)/out/target/product/generic_arm64/obj/kernel/usr

# Output
BPF_OBJ := ebpf_program.o

.PHONY: all clean load unload

all: $(BPF_OBJ)

$(BPF_OBJ): ebpf_program.c
	$(CLANG) \
		-target bpf \
		-D__TARGET_ARCH_arm64 \
		-I$(KERNEL_HEADERS)/include \
		-I$(KERNEL_HEADERS)/include/uapi \
		-O2 -g \
		-Wall -Werror \
		-c $< -o $@

# Load the eBPF program and pin maps
load: $(BPF_OBJ)
	@echo "Loading eBPF program..."
	$(BPFTOOL) prog load $(BPF_OBJ) /sys/fs/bpf/phantom_write type kprobe
	$(BPFTOOL) map pin name offload_ringbuf /sys/fs/bpf/offload_ringbuf
	$(BPFTOOL) map pin name active_sessions /sys/fs/bpf/active_sessions
	@echo "Attaching kprobes..."
	$(BPFTOOL) net attach kprobe name bpf_prog_write /sys/fs/bpf/phantom_write
	@echo "eBPF loaded and attached"

# Unload everything
unload:
	@echo "Unloading eBPF..."
	-rm -f /sys/fs/bpf/phantom_write
	-rm -f /sys/fs/bpf/offload_ringbuf
	-rm -f /sys/fs/bpf/active_sessions
	@echo "eBPF unloaded"

clean:
	rm -f $(BPF_OBJ)
