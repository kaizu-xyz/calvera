# Linux Core Isolation for Low-Latency Threads

Core isolation removes specific CPU cores from the kernel's general scheduler so that **only your pinned threads** run on them. No other processes, kernel workers, or interrupts compete for those cores — eliminating context switches and reducing jitter.

This is a two-step process:

1. **Kernel side**: isolate cores so the OS scheduler ignores them
2. **Application side**: pin your threads to those isolated cores (calvera does this via `core_affinity`)

Without step 1, `core_affinity` is just a preference — the OS can still schedule other work on the same core.

## Step 1: Isolate Cores

### Find your CPU topology

```bash
# How many cores?
nproc

# Detailed topology (cores, NUMA nodes, cache)
lscpu

# Per-core breakdown
cat /proc/cpuinfo | grep -E "processor|core id|physical id"
```

Pick cores to isolate. General rule: keep core 0 for the OS and isolate higher-numbered cores for your application.

### Option A: Boot parameter (persistent, requires reboot)

Edit your bootloader config. For GRUB:

```bash
sudo vim /etc/default/grub
```

Add `isolcpus` and `nohz_full` to the kernel command line:

```
GRUB_CMDLINE_LINUX="isolcpus=2,3 nohz_full=2,3 rcu_nocbs=2,3"
```

- `isolcpus=2,3` — removes cores 2 and 3 from the general scheduler
- `nohz_full=2,3` — disables timer ticks on those cores when they have a single running task (reduces jitter)
- `rcu_nocbs=2,3` — offloads RCU callbacks away from those cores (avoids kernel housekeeping interrupts)

Apply and reboot:

```bash
sudo update-grub   # Debian/Ubuntu
# or
sudo grub2-mkconfig -o /boot/grub2/grub.cfg  # RHEL/CentOS

sudo reboot
```

### Option B: cgroup cpuset (no reboot, less thorough)

```bash
# Create a cpuset for your application
sudo mkdir /sys/fs/cgroup/calvera

# Assign cores 2 and 3
echo "2-3" | sudo tee /sys/fs/cgroup/calvera/cpuset.cpus
echo "0"   | sudo tee /sys/fs/cgroup/calvera/cpuset.mems

# Run your process in that cpuset
sudo cgexec -g cpuset:calvera ./your_binary
```

This restricts which cores your process can use, but doesn't prevent the OS from scheduling other work on those cores. It's weaker than `isolcpus` but doesn't require a reboot.

For true isolation, use Option A.

## Step 2: Pin threads (application side)

Calvera already does this via `core_affinity` when you use `.pin_at_core(N)` in the builder:

```rust
let mut producer = calvera::build_uni_producer_unchecked(64, factory, BusySpin)
    .pin_at_core(2).handle_events_with(handler_a)
    .pin_at_core(3).handle_events_with(handler_b)
    .build();
```

Make sure the core numbers match the isolated cores from step 1.

## Verify isolation is working

After reboot (Option A), verify:

```bash
# Confirm isolated cores
cat /sys/devices/system/cpu/isolated
# Should show: 2-3

# Confirm nohz_full
cat /proc/cmdline | grep -o 'nohz_full=[^ ]*'

# Watch per-core utilisation — isolated cores should be near 0% until your app starts
htop
```

While your application runs, verify thread pinning:

```bash
# Find your process
pidof your_binary

# Check which cores each thread is on
ps -eo pid,tid,psr,comm | grep your_binary
# The PSR column shows which core each thread is running on
```

## Additional tuning

These are optional but can further reduce jitter:

```bash
# Disable irqbalance (prevents interrupts from landing on isolated cores)
sudo systemctl stop irqbalance
sudo systemctl disable irqbalance

# Manually move IRQs away from isolated cores
# For each IRQ in /proc/irq/*/smp_affinity, set the mask to exclude cores 2,3
# Mask for cores 0,1 only on a 4-core system: 0x3
for irq in /proc/irq/*/smp_affinity; do
    echo 3 | sudo tee "$irq" 2>/dev/null
done

# Set the application thread to SCHED_FIFO (real-time priority)
sudo chrt -f 99 ./your_binary
# Or for a running process:
sudo chrt -f -p 99 <tid>
```

## Summary

| What | How | Effect |
|------|-----|--------|
| `isolcpus` | Kernel boot param | Removes cores from general scheduler |
| `nohz_full` | Kernel boot param | Disables timer ticks on idle isolated cores |
| `rcu_nocbs` | Kernel boot param | Offloads kernel callbacks off isolated cores |
| `core_affinity` | Application code | Pins threads to specific cores |
| `irqbalance off` | systemd service | Prevents hardware interrupts on isolated cores |
| `SCHED_FIFO` | `chrt` command | Real-time scheduling priority |

The first three require a reboot. The rest can be applied at runtime.
