#!/bin/sh
set -eu

# The browser must reach the backend through this origin (SameSite=Strict
# session cookie + Origin check), so the target is required.
: "${JANUS_API_TARGET:?JANUS_API_TARGET must be set to the backend origin, e.g. http://janus-server:4317}"

# Substitute only ${JANUS_API_TARGET}; nginx's own $variables ($uri, $http_upgrade,
# ...) are left untouched.
envsubst '${JANUS_API_TARGET}' \
    < /etc/nginx/conf.d/default.conf.template \
    > /etc/nginx/conf.d/default.conf

exec "$@"
