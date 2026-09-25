#!/usr/bin/env bash
set -euo pipefail

container="sql-manager-e2e-$$"
docker run --rm --detach \
  --name "$container" \
  --env POSTGRES_USER=sql_manager \
  --env POSTGRES_PASSWORD=e2e_test_only \
  --env POSTGRES_DB=sql_manager_test \
  --publish 127.0.0.1::5432 \
  postgres:18-alpine >/dev/null

cleanup() {
  docker stop "$container" >/dev/null 2>&1 || true
}
trap cleanup EXIT

port=$(docker port "$container" 5432/tcp | python3 -c 'import sys; print(sys.stdin.read().strip().rsplit(":", 1)[-1])')
for _ in $(seq 1 30); do
  if docker exec "$container" pg_isready -q -U sql_manager -d sql_manager_test; then
    break
  fi
  sleep 1
done
docker exec "$container" pg_isready -U sql_manager -d sql_manager_test

export SQL_MANAGER_E2E_DATABASE_URL="postgresql://sql_manager:e2e_test_only@127.0.0.1:${port}/sql_manager_test?sslmode=disable"
for _ in 1 2 3; do
  cargo test --locked -- --ignored --nocapture
done
