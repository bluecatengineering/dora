#!/bin/bash
# Docker Cleanup and Optimization Script

set -e

# Colors
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

echo -e "${BLUE}=== Docker Cleanup Script ===${NC}"
echo ""

# Show current usage
echo -e "${YELLOW}Current Docker disk usage:${NC}"
sudo docker system df
echo ""

echo -e "${YELLOW}Current system disk usage:${NC}"
df -h / | grep -E "(Filesystem|/dev/)"
echo ""

# Ask for confirmation
read -p "Do you want to clean up Docker? (y/n) " -n 1 -r
echo
if [[ ! $REPLY =~ ^[Yy]$ ]]; then
    echo "Cleanup cancelled."
    exit 0
fi

echo ""
echo -e "${GREEN}Step 1: Removing stopped containers...${NC}"
sudo docker container prune -f

echo ""
echo -e "${GREEN}Step 2: Removing unused images...${NC}"
sudo docker image prune -a -f

echo ""
echo -e "${GREEN}Step 3: Removing unused volumes...${NC}"
sudo docker volume prune -f

echo ""
echo -e "${GREEN}Step 4: Removing build cache...${NC}"
sudo docker builder prune -a -f

echo ""
echo -e "${GREEN}Step 5: Complete system cleanup...${NC}"
sudo docker system prune -a -f --volumes

echo ""
echo -e "${BLUE}=== Cleanup Complete ===${NC}"
echo ""

echo -e "${YELLOW}New Docker disk usage:${NC}"
sudo docker system df
echo ""

echo -e "${YELLOW}New system disk usage:${NC}"
df -h / | grep -E "(Filesystem|/dev/)"
echo ""

echo -e "${GREEN}Cleanup completed successfully!${NC}"
echo ""
echo "Note: Docker images will be re-downloaded when needed."
echo "Cargo build cache in /home/svvs/CRdoraPub/target is preserved."
