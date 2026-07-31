#!/usr/bin/env bash
# Виводить хвіст логу як GitHub-анотацію.
#
# Логи ранів недоступні без авторизації навіть у публічному репозиторії
# («Sign in to view logs»), а анотації — доступні через API:
#   /repos/<OWNER>/<REPO>/check-runs/<JOB_ID>/annotations
#
# ВАЖЛИВО (правило 21a): GitHub ріже повідомлення анотації на 4096 символах
# і відкидає ХВІСТ — тобто саме рядок з помилкою. Шаблон з `tail -c 6000`
# через це мовчить осмислено: у анотацію потрапляють перші 4096 з 6000.
#
#   bash scripts/ci-annotate.sh "Build output" build.log [limit]

set -uo pipefail

title="${1:?потрібен заголовок}"
file="${2:?потрібен файл логу}"
limit="${3:-2500}"

if [ ! -s "$file" ]; then
  echo "::error title=${title}::${file} is missing or empty"
  exit 0
fi

# Знімаємо ANSI-послідовності — кольори cargo з'їдають половину ліміту
# на невидиме.
clean=$(sed -e 's/\x1b\[[0-9;]*m//g' "$file")

# Прогрес cargo/npm ніколи не буває причиною падіння, а рядків цих сотні —
# саме вони виштовхують помилку за межі ліміту. Викидаємо лише цей ВІДОМИЙ
# шум; відбір за очікуваним форматом помилки заборонений правилом 21.
trimmed=$(printf '%s\n' "$clean" \
  | grep -vE '^[[:space:]]*(Compiling|Checking|Downloaded|Downloading|Fresh|Updating|Adding|Locking|Installing|Compressing) ' || true)
# Якщо після чистки не лишилось нічого — показуємо лог як є, інакше крок
# замовкне саме тоді, коли лог складається з самого прогресу.
[ -n "$trimmed" ] && clean="$trimmed"

log=$(printf '%s' "$clean" | tail -c "$limit")

log="${log//'%'/'%25'}"
log="${log//$'\r'/'%0D'}"
log="${log//$'\n'/'%0A'}"

echo "::error title=${title}::${log}"
