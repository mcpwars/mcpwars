#!/bin/sh
# Listen where the platform expects (Render sets PORT), unless AF_BIND says otherwise.
# When started as root, hand the data directory (a mounted volume, if any) to the
# service user and drop privileges; otherwise run as whoever we already are.
set -eu
export AF_BIND="${AF_BIND:-0.0.0.0:${PORT:-8080}}"
if [ "$(id -u)" = "0" ]; then
  chown -R mcpwars:mcpwars /data
  exec setpriv --reuid=mcpwars --regid=mcpwars --init-groups /usr/local/bin/af-server "$@"
fi
exec /usr/local/bin/af-server "$@"
