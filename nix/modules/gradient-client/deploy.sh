#!/usr/bin/env bash

die() {
  echo "Error: $1"
  exit 1
}

if [[ "$GRADIENT_DEPLOY_TYPE" == "microvm" ]]; then
  while IFS= read -r name; do
    vm_dir="${GRADIENT_MICROVM_STATE_DIR}/${name}"
    if [[ -e "${vm_dir}/toplevel" || -L "${vm_dir}/toplevel" ]]; then
      die "MicroVM ${name} is declarative and cannot be deployed imperatively"
    fi
    [[ -d "$vm_dir" ]] || die "MicroVM ${name} is not deployed: state directory $vm_dir does not exist"
  done < <(jq -r '.[]' <<< "$GRADIENT_MICROVM_HOSTNAMES")
fi

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

resolve_microvms() {
  local evaluation_status="$1" entry_points="$2"
  local name entry_point path status vm_dir current booted

  while IFS= read -r name; do
    entry_point="nixosConfigurations.${name}.config.microvm.declaredRunner"
    path=""
    status=""
    IFS=$'\t' read -r path status < <(
      jq -r --arg entry_point "$entry_point" \
        'first(.message.entry_points[] | select(.eval == $entry_point)) | [(.outputs.out // ""), (.build_status // "")] | @tsv' \
        <<< "$entry_points"
    ) || true

    if [[ -z "$path" ]]; then
      case "$evaluation_status" in
        Completed|Failed|Aborted)
          printf 'done\tNo deployment entry point for MicroVM %s in completed evaluation\n' "$name"
          ;;
        *)
          printf 'wait\n'
          ;;
      esac
      continue
    fi

    vm_dir="${GRADIENT_MICROVM_STATE_DIR}/${name}"
    current=$(readlink -f "${vm_dir}/current" 2>/dev/null || true)
    booted=$(readlink -f "${vm_dir}/booted" 2>/dev/null || true)
    if [[ "$path" == "$current" && "$path" == "$booted" ]] \
      && systemctl is-active --quiet "microvm@${name}.service"; then
      printf 'done\tMicroVM %s is already up-to-date with %s\n' "$name" "$path"
      continue
    fi

    case "$status" in
      Completed|Substituted)
        printf 'deploy\t%s\t%s\n' "$name" "$path"
        ;;
      FailedPermanent|FailedTimeout|DependencyFailed|Aborted)
        printf 'done\tBuild of MicroVM %s runner %s finished %s\n' "$name" "$path" "$status"
        ;;
      *)
        case "$evaluation_status" in
          Completed|Failed|Aborted)
            printf 'done\tMicroVM %s runner %s was still %s when evaluation finished %s\n' \
              "$name" "$path" "$status" "$evaluation_status"
            ;;
          *)
            printf 'wait\n'
            ;;
        esac
        ;;
    esac
  done < <(jq -r '.[]' <<< "$GRADIENT_MICROVM_HOSTNAMES")
}

resolve() {
  local evaluation entry_points evaluation_id evaluation_status path status current booted

  evaluation=$(api "${GRADIENT_API_URL}/tasks/${GRADIENT_TASK}/evaluations?limit=1") || { echo "wait"; return; }
  evaluation_id=$(jq -r '.message[0].id // empty' <<< "$evaluation")
  evaluation_status=$(jq -r '.message[0].status // empty' <<< "$evaluation")

  if [[ -z "$evaluation_id" ]]; then
    echo "done task ${GRADIENT_TASK} has no evaluations"
    return
  fi

  entry_points=$(api "${GRADIENT_API_URL}/tasks/${GRADIENT_TASK}/entry-points?evaluation_id=$evaluation_id&limit=500") || { echo "wait"; return; }

  if [[ "$GRADIENT_DEPLOY_TYPE" == "microvm" ]]; then
    resolve_microvms "$evaluation_status" "$entry_points"
    return
  fi

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

deploy() {
  echo "New deployment found: $1"

  nix-store --realize "$1" || die "cannot fetch $1 from a substituter"
  nix-env -p /nix/var/nix/profiles/system --set "$1" || die "cannot set the profile to $1"
  "$1/bin/switch-to-configuration" switch || die "switching to $1 failed"

  echo "Deployment to $1 completed successfully"
}

deploy_microvm() {
  local name="$1"
  local runner="$2"
  local vm_dir="${GRADIENT_MICROVM_STATE_DIR}/${name}"
  local current="${vm_dir}/current"
  local old="${vm_dir}/old"
  local gcroot_dir="/nix/var/nix/gcroots/microvm"
  local temporary old_target

  nix-store --realize "$runner" || die "cannot fetch $runner from a substituter"
  [[ -x "${runner}/bin/microvm-run" ]] || die "MicroVM runner $runner has no executable bin/microvm-run"

  old_target=$(readlink -f "$current" 2>/dev/null || true)
  if [[ -n "$old_target" && "$old_target" != "$runner" ]]; then
    ln -sfnT "$old_target" "$old" || die "cannot preserve previous MicroVM runner $old_target"
    ln -sfnT "$old" "${gcroot_dir}/old-${name}" \
      || die "cannot preserve GC root for previous MicroVM runner $old_target"
  fi

  temporary="${vm_dir}/.current.$$"
  ln -s "$runner" "$temporary" || die "cannot prepare MicroVM runner link for $runner"
  if ! mv -Tf "$temporary" "$current"; then
    rm -f "$temporary"
    die "cannot activate MicroVM runner $runner"
  fi
  ln -sfnT "$current" "${gcroot_dir}/${name}" \
    || die "cannot preserve GC root for MicroVM runner $runner"

  echo "Restarting MicroVM ${name}..."
  systemctl restart "microvm@${name}.service" \
    || die "restarting MicroVM ${name} failed; previous runner is preserved at $old"

  echo "Deployment of MicroVM ${name} to $runner completed successfully"
}

settle() {
  local verdict
  verdict=$(resolve)

  if [[ "$GRADIENT_DEPLOY_TYPE" == "microvm" ]]; then
    local action name path pending=0
    while IFS=$'\t' read -r action name path; do
      case "$action" in
        deploy)
          echo "New deployment found for MicroVM $name: $path"
          deploy_microvm "$name" "$path"
          ;;
        done)
          echo "$name"
          ;;
        wait)
          pending=1
          ;;
      esac
    done <<< "$verdict"
    (( pending )) && return 1
    return 0
  fi

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
