#!/bin/bash
# Immediate Space Recovery Script

set -e

GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m'

echo -e "${YELLOW}=== Immediate Space Recovery ===${NC}"
echo ""

# Show current space
echo "Current disk usage:"
df -h / | grep -E "(Filesystem|/dev/)"
echo ""

echo -e "${GREEN}Step 1: Cleaning Cargo build cache (largest culprit - 12GB)${NC}"
cd /home/svvs/CRdoraPub
echo "Before:"
du -sh target/
cargo clean
echo "After cleanup: target/ removed"
echo ""

echo -e "${GREEN}Step 2: Cleaning Docker images and cache${NC}"
sudo docker system prune -a -f --volumes
echo ""

echo -e "${GREEN}Step 3: Cleaning system package cache${NC}"
sudo apt clean
sudo apt autoremove -y
echo ""

echo -e "${YELLOW}New disk usage:${NC}"
df -h / | grep -E "(Filesystem|/dev/)"
echo ""

echo -e "${GREEN}✓ Space recovery complete!${NC}"
echo ""
echo "Recovered space breakdown:"
echo "  - Cargo target/: ~12GB"
echo "  - Docker cache: ~1-2GB"
echo "  - APT cache: ~100-500MB"
echo ""
echo "Note: Dependencies will be re-downloaded on next build."
