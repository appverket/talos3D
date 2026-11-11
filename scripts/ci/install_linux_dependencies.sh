#!/usr/bin/env bash
# Install Bevy's build dependencies on GitHub's Ubuntu runners.
set -euo pipefail
# The runner's default mirror list uses three names under ubuntu.com. If
# that DNS zone is unavailable, keep Ubuntu signature verification and use
# an independent mirror of the very same archive.
if [[ -f /etc/apt/apt-mirrors.txt ]] && ! getent ahostsv4 archive.ubuntu.com >/dev/null; then
  echo 'archive.ubuntu.com is unavailable; using the kernel.org Ubuntu mirror'
  echo 'https://mirrors.edge.kernel.org/ubuntu/' | sudo tee /etc/apt/apt-mirrors.txt >/dev/null
fi
sudo apt-get update
sudo apt-get install -y libasound2-dev libudev-dev libwayland-dev libxkbcommon-dev
