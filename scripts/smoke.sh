#!/usr/bin/env bash
set -euo pipefail

base_url=${1:-http://127.0.0.1:8080}
expected_ready=${2:-200}
body=$(mktemp)
headers=$(mktemp)
trap 'rm -f "$body" "$headers"' EXIT

check() {
    local path=$1 expected=$2 content_type=$3 actual
    actual=$(curl -sS --max-time 5 -D "$headers" -o "$body" -w '%{http_code}' "$base_url$path")
    if [[ "$actual" != "$expected" ]] || ! grep -qi "^content-type: $content_type" "$headers"; then
        printf '%s returned HTTP %s (expected %s %s)\n' "$path" "$actual" "$expected" "$content_type" >&2
        return 1
    fi
}

check /health 200 application/json
check /api/ready "$expected_ready" application/json
check /api/missing 404 application/json
request_id=$(awk 'tolower($1) == "x-request-id:" {print $2}' "$headers" | tr -d '\r')
[[ -n "$request_id" ]] && grep -Fq "$request_id" "$body"
check /api 404 application/json
check /uploads 404 text/html
check /_protected/files/example 404 text/html
check /some/client/route 200 text/html
grep -Fq 'id="app"' "$body"

printf 'Smoke checks passed (%s, readiness %s)\n' "$base_url" "$expected_ready"
