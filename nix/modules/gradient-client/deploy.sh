#!/usr/bin/env bash

if ! curl --silent --fail --max-time 5 --output /dev/null "${GRADIENT_API_URL}/health"; then
  echo "Error: cannot connect to ${GRADIENT_API_URL}/health"
  exit 1
fi

if ! API_KEY=$(<"${GRADIENT_API_KEY}"); then
  echo "Error: cannot read API key credential ${GRADIENT_API_KEY}"
  exit 1
fi

api() {
  curl --silent --fail --max-time 10 --header "Authorization: Bearer $API_KEY" "$@"
}

resolve() {
  local evaluation entry_points evaluation_id evaluation_status path status current

  evaluation=$(api "${GRADIENT_API_URL}/tasks/${GRADIENT_TASK}/evaluations?limit=1") || { echo "wait"; return; }
  evaluation_id=$(jq -r '.message[0].id // empty' <<< "$evaluation")
  evaluation_status=$(jq -r '.message[0].status // empty' <<< "$evaluation")

  if [[ -z "$evaluation_id" ]]; then
    echo "done task ${GRADIENT_TASK} has no evaluations"
    return
  fi

  entry_points=$(api "${GRADIENT_API_URL}/tasks/${GRADIENT_TASK}/entry-points?evaluation_id=$evaluation_id&limit=500") || { echo "wait"; return; }

  # Output paths are written at evaluation time from the resolved .drv. The deployment is
  # identifiable before its build. Entry points are absent until derivations are resolved.
  IFS=$'\t' read -r path status < <(
    jq -r --arg re "$GRADIENT_SYSTEM_PATH_REGEX" \
      'first(.message.entry_points[] | select((.outputs.out // "") | test($re))) | [(.outputs.out // ""), (.build_status // "")] | @tsv' \
      <<< "$entry_points"
  )

  if [[ -n "$path" ]]; then
    current=$(readlink /run/current-system || true)
    if [[ "$path" == "$current" ]]; then
      echo "done system is already up-to-date with $path"
      return
    fi

    case "$status" in
      Completed|Substituted)
        echo "deploy $path"
        return
        ;;
      FailedPermanent|FailedTimeout|DependencyFailed|Aborted)
        echo "done build of $path finished $status"
        return
        ;;
    esac
  fi

  case "$evaluation_status" in
    Completed|Failed|Aborted)
      echo "done evaluation $evaluation_id finished: $evaluation_status without a deployment for ${GRADIENT_DEPLOY_FOR}"
      ;;
    *)
      echo "wait"
      ;;
  esac
}

die() {
  echo "Error: $1"
  exit 1
}

deploy() {
  echo "New deployment found: $1"

  nix-store --realize "$1" || die "cannot fetch $1 from a substituter"
  nix-env -p /nix/var/nix/profiles/system --set "$1" || die "cannot set the profile to $1"
  "$1/bin/switch-to-configuration" switch || die "switching to $1 failed"

  echo "Deployment to $1 completed successfully"
}

settle() {
  local verdict
  verdict=$(resolve)

  case "$verdict" in
    "deploy "*)
      deploy "${verdict#deploy }"
      ;;
    "done "*)
      echo "${verdict#done }"
      ;;
    *)
      return 1
      ;;
  esac
}

if settle; then
  exit 0
fi

if [[ "${GRADIENT_WAIT_FOR_BUILD}" != 1 ]]; then
  echo "Nothing deployable yet for task ${GRADIENT_TASK}; not waiting"
  exit 0
fi

if [[ "${GRADIENT_WEBSOCKETS}" == 1 ]]; then
  echo "Task ${GRADIENT_TASK} is still building; following ${GRADIENT_LIVE_URL}"

  while true; do
    exec 3< <(websocat -U --text --no-close --ping-interval 30 --ping-timeout 90 \
      -H="Authorization: Bearer $API_KEY" "${GRADIENT_LIVE_URL}" </dev/null)
    stream=$!

    # The socket only reports transitions after the connect. A reconnect gap replays
    # nothing, and every connect must settle again.
    if settle; then
      exit 0
    fi

    while read -r _event <&3; do
      if settle; then
        exit 0
      fi
    done

    exec 3<&-
    kill "$stream" 2>/dev/null || true
    wait "$stream" 2>/dev/null || true
    sleep 30
  done
else
  echo "Task ${GRADIENT_TASK} is still building; re-checking every ${GRADIENT_POLL_INTERVAL_SEC}s"

  while true; do
    sleep "${GRADIENT_POLL_INTERVAL_SEC}"

    if settle; then
      exit 0
    fi
  done
fi
