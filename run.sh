#!/bin/sh
RELEASE_URL="https://github.com/g33kex/ServerWitch/releases/download/v0.1.1/serverwitch-0.1.1-x86_64-linux"

trap 'rm -f "/tmp/exec.$$"' 0
trap 'exit $?' 1 2 3 15

curl $RELEASE_URL -L -s -o /tmp/exec.$$

chmod +x /tmp/exec.$$

/tmp/exec.$$
