#!/usr/bin/env bash
# 部署後的基本檢查：./scripts/smoke_test.sh https://<service-url>
set -uo pipefail
URL="${1:?usage: smoke_test.sh <service-url>}"
fail=0
check() {
  local desc="$1" expected="$2" actual="$3"
  if [[ "$actual" == "$expected" ]]; then echo "  ✅ $desc ($actual)"; else echo "  ❌ $desc: expected $expected, got $actual"; fail=1; fi
}
code() { curl -s -o /dev/null -w '%{http_code}' "$@"; }

echo "==> Smoke testing ${URL}"
check "GET / health"               200 "$(code "$URL/")"
check "GET /liff/"                 200 "$(code "$URL/liff/")"
check "GET /static/liff/voice.js"  200 "$(code "$URL/static/liff/voice.js")"
check "GET /reports/not-exist"     404 "$(code "$URL/reports/not-exist")"
check "GET /images/not-exist"      404 "$(code "$URL/images/not-exist")"
check "POST / bad signature"       400 "$(code -X POST -H 'x-line-signature: bad' -d '{"events":[]}' "$URL/")"
check "POST /urls empty"           400 "$(code -X POST -H 'content-type: application/json' -d '{"urls":[]}' "$URL/urls")"
exit $fail
