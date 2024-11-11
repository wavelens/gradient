#!/bin/sh

# Copyright 2022 SAP SE
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

# shellcheck shell=ash
set -euo pipefail

# Darwin compatibility
if hash greadlink >/dev/null 2>/dev/null; then
  readlink() { greadlink "$@"; }
fi

step() {
  printf '\x1B[1;36m>>\x1B[0;36m %s...\x1B[0m\n' "$1"
}

if [ ! -d testing/postgresql-data/ ]; then
  step "First-time setup: Creating PostgreSQL database for testing"
  initdb -A trust -U postgres testing/postgresql-data/
fi
mkdir -p testing/postgresql-run/

step "Configuring PostgreSQL"
sed -ie '/^#\?\(external_pid_file\|unix_socket_directories\|port\)\b/d' testing/postgresql-data/postgresql.conf
(
  echo "external_pid_file = '${PWD}/testing/postgresql-run/pid'"
  echo "unix_socket_directories = '${PWD}/testing/postgresql-run'"
  echo "port = 54321"
) >> testing/postgresql-data/postgresql.conf

# usage in trap is not recognized
# shellcheck disable=SC2317
stop_postgres() {
  EXIT_CODE=$?
  step "Stopping PostgreSQL"
  pg_ctl stop -D testing/postgresql-data/ -w -s
  rm -rf testing
  exit "${EXIT_CODE}"
}

step "Starting PostgreSQL"
rm -f -- testing/postgresql.log
trap stop_postgres EXIT INT TERM
pg_ctl start -D testing/postgresql-data/ -l testing/postgresql.log -w -s
createdb -U postgres -h localhost -p 54321 gradient

step "Running command: $*"
set +e
"$@"
EXIT_CODE=$?
set -e

exit "${EXIT_CODE}"

