#!/usr/bin/env bash
# 建置映像檔並部署到 Cloud Run。
# 環境變數直接從現有的 Python 版服務複製（只寫進暫存檔，不輸出到終端機）。
#
#   ./scripts/deploy.sh                       # 部署（Batch 輪詢預設關閉）
#   ENABLE_BATCH_POLLING=true ./scripts/deploy.sh   # 切換 LINE webhook 後再開
set -euo pipefail

PROJECT="${PROJECT:-$(gcloud config get-value project 2>/dev/null)}"
REGION="${REGION:-us-central1}"
SERVICE="${SERVICE:-linebot-helper-rust}"
SOURCE_SERVICE="${SOURCE_SERVICE:-linebot-helper-python}"
ENABLE_BATCH_POLLING="${ENABLE_BATCH_POLLING:-false}"
TAG="$(git rev-parse --short HEAD)"
IMAGE="${REGION}-docker.pkg.dev/${PROJECT}/cloud-run-source-deploy/${SERVICE}:${TAG}"

cd "$(dirname "$0")/.."

echo "==> Building ${IMAGE}"
gcloud builds submit --project "$PROJECT" --config cloudbuild.yaml \
  --substitutions "_REGION=${REGION},_IMAGE=${SERVICE},_TAG=${TAG}" .

ENV_FILE="$(mktemp -t "${SERVICE}-env.XXXXXX")"
trap 'rm -f "$ENV_FILE"' EXIT
chmod 600 "$ENV_FILE"

echo "==> Copying environment variables from ${SOURCE_SERVICE}"
gcloud run services describe "$SOURCE_SERVICE" --project "$PROJECT" --region "$REGION" --format=json \
  | python3 -c '
import json, sys
env = json.load(sys.stdin)["spec"]["template"]["spec"]["containers"][0].get("env", [])
out = {e["name"]: e["value"] for e in env if "value" in e}
out["ENABLE_BATCH_POLLING"] = sys.argv[1]
json.dump(out, open(sys.argv[2], "w"))
print(f"    {len(out)} variables: {sorted(out)}")
' "$ENABLE_BATCH_POLLING" "$ENV_FILE"

SA="$(gcloud run services describe "$SOURCE_SERVICE" --project "$PROJECT" --region "$REGION" \
  --format='value(spec.template.spec.serviceAccountName)')"

echo "==> Deploying ${SERVICE}"
gcloud run deploy "$SERVICE" \
  --project "$PROJECT" \
  --region "$REGION" \
  --image "$IMAGE" \
  --env-vars-file "$ENV_FILE" \
  --service-account "$SA" \
  --allow-unauthenticated \
  --no-cpu-throttling \
  --cpu-boost \
  --cpu 2 \
  --memory 2Gi \
  --concurrency 80 \
  --timeout 300 \
  --max-instances 50

URL="$(gcloud run services describe "$SERVICE" --project "$PROJECT" --region "$REGION" --format='value(status.url)')"
echo "==> Deployed: ${URL}"
"$(dirname "$0")/smoke_test.sh" "$URL"
