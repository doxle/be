#!/usr/bin/env bash
set -euo pipefail

# Run the API lambda locally (cargo lambda watch) with backup/import/reconcile orchestration env vars.
#
# This script fetches Step Functions ARNs for backup/import and verifies the reconcile worker
# Lambda target so local async jobs can run without redeploying Lambda code each time.
#
# Usage:
#   ./scripts/run_local_api_with_backup.sh
#
# Optional env overrides:
#   AWS_REGION=ap-southeast-2
#   STATE_MACHINE_NAME=doxle-block-backup
#   IMPORT_STATE_MACHINE_NAME=doxle-block-import
#   RECONCILE_WORKER_FUNCTION_NAME=doxle-reconcile-worker:prod
#   TABLE_NAME=doxle
#   S3_BUCKET_NAME=doxle-app

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BE_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

AWS_REGION="${AWS_REGION:-${AWS_DEFAULT_REGION:-ap-southeast-2}}"
STATE_MACHINE_NAME="${STATE_MACHINE_NAME:-doxle-block-backup}"
IMPORT_STATE_MACHINE_NAME="${IMPORT_STATE_MACHINE_NAME:-doxle-block-import}"
RECONCILE_WORKER_FUNCTION_NAME="${RECONCILE_WORKER_FUNCTION_NAME:-doxle-reconcile-worker:prod}"
TABLE_NAME="${TABLE_NAME:-doxle}"
S3_BUCKET_NAME="${S3_BUCKET_NAME:-doxle-app}"

if ! command -v aws >/dev/null 2>&1; then
  echo "❌ AWS CLI not found. Install/configure AWS CLI first." >&2
  exit 1
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "❌ cargo not found." >&2
  exit 1
fi

if ! cargo lambda --help >/dev/null 2>&1; then
  echo "❌ cargo-lambda is not installed. Install with: cargo install cargo-lambda" >&2
  exit 1
fi

ACCOUNT_ID=$(AWS_PAGER="" AWS_REGION="${AWS_REGION}" aws sts get-caller-identity --query Account --output text 2>/dev/null || true)
if [[ -z "${ACCOUNT_ID}" || "${ACCOUNT_ID}" == "None" ]]; then
  echo "❌ AWS credentials are not configured for region ${AWS_REGION}." >&2
  exit 1
fi

BACKUP_STATE_MACHINE_ARN=$(AWS_PAGER="" AWS_REGION="${AWS_REGION}" aws stepfunctions list-state-machines \
  --query "stateMachines[?name=='${STATE_MACHINE_NAME}'].stateMachineArn | [0]" \
  --output text)

if [[ -z "${BACKUP_STATE_MACHINE_ARN}" || "${BACKUP_STATE_MACHINE_ARN}" == "None" ]]; then
  echo "❌ Step Functions state machine '${STATE_MACHINE_NAME}' not found in ${AWS_REGION}." >&2
  echo "   Create it first (already done in this environment as 'doxle-block-backup')." >&2
  exit 1
fi

IMPORT_STATE_MACHINE_ARN=$(AWS_PAGER="" AWS_REGION="${AWS_REGION}" aws stepfunctions list-state-machines \
  --query "stateMachines[?name=='${IMPORT_STATE_MACHINE_NAME}'].stateMachineArn | [0]" \
  --output text)

if [[ -z "${IMPORT_STATE_MACHINE_ARN}" || "${IMPORT_STATE_MACHINE_ARN}" == "None" ]]; then
  echo "❌ Step Functions state machine '${IMPORT_STATE_MACHINE_NAME}' not found in ${AWS_REGION}." >&2
  echo "   Create it first (already done in this environment as 'doxle-block-import')." >&2
  exit 1
fi

if ! AWS_PAGER="" AWS_REGION="${AWS_REGION}" aws lambda get-function \
  --function-name "${RECONCILE_WORKER_FUNCTION_NAME}" \
  --query 'Configuration.FunctionArn' \
  --output text >/dev/null 2>&1; then
  echo "❌ Reconcile worker Lambda '${RECONCILE_WORKER_FUNCTION_NAME}' not found in ${AWS_REGION}." >&2
  exit 1
fi

export AWS_REGION
export TABLE_NAME
export S3_BUCKET_NAME
export BACKUP_STATE_MACHINE_ARN
export IMPORT_STATE_MACHINE_ARN
export RECONCILE_WORKER_FUNCTION_NAME
export IS_LOCAL=true
export COGNITO_CLIENT_ID="${COGNITO_CLIENT_ID:-59u7sgmc3u137f960q2k8j0lc8}"
export COGNITO_CLIENT_SECRET="${COGNITO_CLIENT_SECRET:-1cp4p05hq1iv10oacjtu1e0k7betc0dgrnsr5torka52al7essra}"
export COGNITO_USER_POOL_ID="${COGNITO_USER_POOL_ID:-ap-southeast-2_Rmd3TFvE9}"
echo "✅ Local API backup/import/reconcile env ready"
echo "   AWS_REGION=${AWS_REGION}"
echo "   TABLE_NAME=${TABLE_NAME}"
echo "   S3_BUCKET_NAME=${S3_BUCKET_NAME}"
echo "   BACKUP_STATE_MACHINE_ARN=${BACKUP_STATE_MACHINE_ARN}"
echo "   IMPORT_STATE_MACHINE_ARN=${IMPORT_STATE_MACHINE_ARN}"
echo "   RECONCILE_WORKER_FUNCTION_NAME=${RECONCILE_WORKER_FUNCTION_NAME}"
echo "   Starting local API on http://localhost:9000 ..."

exec cargo lambda watch --manifest-path "${BE_ROOT}/Cargo.toml" --package doxle-api-lambda
