#!/usr/bin/env bash
set -euo pipefail

# Create or update the Step Functions state machine used by async block imports.
#
# Required:
#   WORKER_FUNCTION_ARN  - Lambda ARN used by Step Functions (function or alias ARN)
#
# Optional:
#   STATE_MACHINE_NAME   - defaults to doxle-block-import
#   ROLE_ARN             - required only when creating the state machine

if [[ -z "${WORKER_FUNCTION_ARN:-}" ]]; then
  echo "❌ WORKER_FUNCTION_ARN is required" >&2
  exit 1
fi

STATE_MACHINE_NAME=${STATE_MACHINE_NAME:-doxle-block-import}
ROLE_ARN=${ROLE_ARN:-}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BE_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
ASL_TEMPLATE="${BE_ROOT}/stepfunctions/block_import_state_machine.asl.json"

if [[ ! -f "${ASL_TEMPLATE}" ]]; then
  echo "❌ ASL template not found: ${ASL_TEMPLATE}" >&2
  exit 1
fi

echo "[1/3] Rendering state machine definition..."
DEFINITION=$(WORKER_FUNCTION_ARN="${WORKER_FUNCTION_ARN}" ASL_TEMPLATE="${ASL_TEMPLATE}" python3 - <<'PY'
import json
import os
from pathlib import Path

template_path = Path(os.environ["ASL_TEMPLATE"])
worker_arn = os.environ["WORKER_FUNCTION_ARN"]
content = template_path.read_text()
content = content.replace("${WORKER_FUNCTION_ARN}", worker_arn)
json.loads(content)  # validate JSON
print(content)
PY
)

echo "[2/3] Looking up existing state machine '${STATE_MACHINE_NAME}'..."
EXISTING_ARN=$(aws stepfunctions list-state-machines \
  --query "stateMachines[?name=='${STATE_MACHINE_NAME}'].stateMachineArn | [0]" \
  --output text)

if [[ "${EXISTING_ARN}" == "None" || -z "${EXISTING_ARN}" ]]; then
  if [[ -z "${ROLE_ARN}" ]]; then
    echo "❌ ROLE_ARN is required to create a new state machine" >&2
    exit 1
  fi
  echo "[3/3] Creating state machine..."
  STATE_MACHINE_ARN=$(aws stepfunctions create-state-machine \
    --name "${STATE_MACHINE_NAME}" \
    --definition "${DEFINITION}" \
    --role-arn "${ROLE_ARN}" \
    --type STANDARD \
    --query 'stateMachineArn' --output text)
else
  echo "[3/3] Updating state machine..."
  aws stepfunctions update-state-machine \
    --state-machine-arn "${EXISTING_ARN}" \
    --definition "${DEFINITION}" \
    --query 'updateDate' --output text >/dev/null
  STATE_MACHINE_ARN="${EXISTING_ARN}"
fi

echo "✅ State machine ARN: ${STATE_MACHINE_ARN}"
echo "Set this env var on the API Lambda:"
echo "IMPORT_STATE_MACHINE_ARN=${STATE_MACHINE_ARN}"
