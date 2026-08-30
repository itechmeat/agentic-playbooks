#!/bin/sh
# Harmless fixture script for the Paperclip apb connector.
# No network, no writes outside stdout. Echoes the run marker.
echo "apb-noop fixture OK"
echo "pwd=$(pwd)"
echo "ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
exit 0
