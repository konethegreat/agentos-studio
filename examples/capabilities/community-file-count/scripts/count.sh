#!/bin/sh
# Count files per extension under the current folder.
find . -type f -name '*.*' | sed 's/.*\.//' | sort | uniq -c | sort -rn
