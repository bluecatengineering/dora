# Docker Memory and Disk Management

## Current System Status

- **Total Memory**: 7.87GB
- **Total Disk**: 15GB
- **Disk Used**: 9.7GB (72%)
- **Disk Available**: 3.9GB
- **Docker Root**: `/home/svvs/.local/share/docker` (rootless mode)

## Problem

The workflow failures were due to **insufficient disk space**, not memory. With only 3.9GB free, Docker runs out of space during large compilations.

## Solutions

### Option 1: Clean Up Docker (Quick Fix) ⭐ RECOMMENDED

Run the automated cleanup script:
```bash
../scripts/docker-cleanup.sh
```

Or manually:
```bash
# Remove all stopped containers
sudo docker container prune -f

# Remove all unused images
sudo docker image prune -a -f

# Remove all unused volumes
sudo docker volume prune -f

# Remove build cache
sudo docker builder prune -a -f

# Complete cleanup (careful: removes everything unused)
sudo docker system prune -a -f --volumes
```

**Expected space recovery**: ~2-3GB

### Option 2: Clean Up Cargo Build Cache

The `target/` directory can grow large:
```bash
# Check size
du -sh /home/svvs/CRdoraPub/target

# Clean it
cd /home/svvs/CRdoraPub
cargo clean

# Or just remove release builds
rm -rf target/release
```

**Expected space recovery**: 1-2GB

### Option 3: Increase Disk Space

Since you're on a Raspberry Pi (likely), you have options:

#### A. Expand SD Card Partition
If your SD card is larger than 15GB:
```bash
# Check available space on SD card
sudo fdisk -l /dev/mmcblk0

# Expand partition (if space available)
sudo raspi-config
# Navigate to: Advanced Options → Expand Filesystem
# Reboot

# Or manually:
sudo growpart /dev/mmcblk0 2
sudo resize2fs /dev/mmcblk0p2
```

#### B. Use External Storage
Mount an external drive for Docker:
```bash
# Stop Docker
sudo systemctl stop docker

# Move Docker data
sudo mv /home/svvs/.local/share/docker /mnt/external/docker

# Create symlink
ln -s /mnt/external/docker /home/svvs/.local/share/docker

# Start Docker
sudo systemctl start docker
```

#### C. Configure Docker Data Root (requires Docker restart)
Create `/etc/docker/daemon.json`:
```json
{
  "data-root": "/mnt/external/docker"
}
```

Then:
```bash
sudo systemctl restart docker
```

### Option 4: Optimize Workflow Execution

Run workflows with cleanup between jobs:
```bash
# Run one workflow at a time with cleanup
sudo act -j fmt push
sudo docker system prune -f
sudo act -j clippy push
sudo docker system prune -f
```

Or use the `--rm` flag (automatic cleanup):
```bash
sudo act -j fmt push --rm
```

## Memory Management (For Reference)

Your system has 7.87GB RAM which is sufficient. Docker in rootless mode doesn't support memory limits, but you can:

### Monitor Memory Usage
```bash
# System memory
free -h

# Docker container memory
sudo docker stats

# Per-container limits (if running Docker as root)
sudo docker run --memory="4g" --memory-swap="4g" ...
```

### For Root Docker (not rootless)
Edit `/etc/docker/daemon.json`:
```json
{
  "default-ulimits": {
    "memlock": {
      "Hard": -1,
      "Name": "memlock",
      "Soft": -1
    }
  }
}
```

## Recommended Workflow

### Before Running Workflows:
```bash
# 1. Check available space
df -h /

# 2. Clean up if needed
../scripts/docker-cleanup.sh

# 3. Run workflow
sudo act -j fmt push
```

### Regular Maintenance:
```bash
# Weekly: Clean Docker
sudo docker system prune -a -f

# Monthly: Clean Cargo cache
cd /home/svvs/CRdoraPub
cargo clean

# Check disk usage
sudo docker system df
df -h /
```

## Quick Commands Reference

### Check Space
```bash
# Disk space
df -h /

# Docker usage
sudo docker system df

# Detailed Docker usage
sudo docker system df -v

# Cargo target size
du -sh /home/svvs/CRdoraPub/target
```

### Clean Up
```bash
# Quick Docker cleanup
sudo docker system prune -f

# Aggressive cleanup (removes everything unused)
sudo docker system prune -a -f --volumes

# Cargo cleanup
cargo clean

# Remove only build artifacts, keep dependencies
cargo clean --release
```

### Monitor
```bash
# Watch disk space
watch -n 5 'df -h /'

# Watch Docker during builds
sudo docker stats

# Watch system resources
htop
```

## Troubleshooting

### "No space left on device" during build
1. Run cleanup script: `../scripts/docker-cleanup.sh`
2. Clean cargo cache: `cargo clean`
3. Remove old kernels: `sudo apt autoremove`
4. Clear apt cache: `sudo apt clean`

### Docker fails to start after moving data
1. Check symlink: `ls -la /home/svvs/.local/share/docker`
2. Check permissions: `ls -la /mnt/external/docker`
3. Fix ownership: `sudo chown -R svvs:svvs /mnt/external/docker`

### Out of memory (less common)
1. Check swap: `free -h`
2. Enable more swap if needed
3. Reduce parallel jobs: `export CARGO_BUILD_JOBS=2`

## Automation

### Automatic Cleanup Before Workflows

Create `.github/workflows/cleanup.yml`:
```yaml
name: Pre-build Cleanup
on: [push]
jobs:
  cleanup:
    runs-on: ubuntu-latest
    steps:
      - name: Clean Docker
        run: docker system prune -f
```

Or add to your test script:
```bash
#!/bin/bash
# Cleanup before tests
sudo docker system prune -f
sudo act -j fmt push
```

## Best Practices

1. ✅ Run `../scripts/docker-cleanup.sh` weekly
2. ✅ Monitor disk usage: `df -h /`
3. ✅ Clean cargo cache after major builds: `cargo clean`
4. ✅ Use external storage if available
5. ✅ Run one heavy workflow at a time
6. ⚠️ Don't keep too many old Docker images
7. ⚠️ Avoid building debug and release simultaneously

## Summary

**Immediate action**: Run `../scripts/docker-cleanup.sh` to free 2-3GB
**Long-term**: Expand disk or use external storage
**Monitoring**: Check `df -h /` and `docker system df` regularly
