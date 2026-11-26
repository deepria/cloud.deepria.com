# Deepria Cloud

`cloud.deepria.com`의 Rust API와 Vue 웹 개발환경입니다. 현재 화면에서 API 및 PostgreSQL 연결 상태를 확인할 수 있습니다.

## 로컬 실행

Docker와 Compose가 필요합니다. macOS에서 Colima를 사용하는 경우 시작한 Docker 컨텍스트를 선택합니다. 이 작업용 Colima 컨텍스트를 직접 지정하려면 각 명령의 `docker` 뒤에 `--context colima-deepria-cloud`를 붙입니다. Homebrew의 Compose와 Buildx가 Docker에 표시되지 않으면 Docker 설정에 `/opt/homebrew/lib/docker/cli-plugins`를 플러그인 디렉터리로 추가합니다.

처음 한 번 프로젝트 루트에서 빈 `.env.example`을 참고해 로컬 DB 비밀번호를 생성합니다. 기존 `.env`가 있으면 해당 파일을 그대로 사용합니다.

```sh
(umask 077 && set -C && printf 'LOCAL_DB_PASSWORD=%s\n' "$(openssl rand -hex 24)" > .env)
docker compose up --build -d --wait
```

브라우저에서 <http://127.0.0.1:8080>을 엽니다. 포트가 사용 중이면 `.env`의 `WEB_PORT`를 설정합니다. PostgreSQL과 Rust API는 Compose 내부에서 접근합니다. 로컬 DB 초기화와 앱 스키마는 컨테이너 기동 시 적용되며 데이터는 Compose 볼륨에 남습니다. 서버를 중지하고 컨테이너를 내려도 볼륨은 보존됩니다.

```sh
scripts/smoke.sh
docker compose down
```

`scripts/smoke.sh`에 주소를 넘기면 다른 로컬 포트에서도 점검할 수 있습니다. `/health`는 API 생존 여부를, `/api/ready`는 PostgreSQL 연결 및 현재 스키마·마이그레이션 이력을 보여줍니다. `/api/ready`의 DB 장애 응답은 HTTP 503입니다.

## 코드 변경과 검사

Rust 버전은 `rust-toolchain.toml`, Node 버전은 `.nvmrc`에 고정합니다. Vue에서 변경을 바로 확인하려면 Compose를 실행한 상태에서 아래 명령으로 개발 서버를 엽니다. Vite는 API 요청을 로컬 Nginx로 전달합니다.

```sh
cd apps/web
npm ci
npm run dev
```

브라우저에서 <http://127.0.0.1:5173>을 엽니다. Compose의 포트를 바꿨으면 `CLOUD_API_PROXY`를 해당 로컬 주소로 설정합니다. Rust API를 macOS에서 직접 실행할 때는 `CLOUD_API_PROXY=http://127.0.0.1:3000`을 사용합니다.

아래 검사는 프로젝트 루트에서 실행합니다.

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
(cd apps/web && npm ci && npm run lint && npm run typecheck && npm test && npm run build)
```

## 환경변수

| 설정 | 사용처 |
| --- | --- |
| `LOCAL_DB_PASSWORD` | 로컬 Compose의 DB 비밀번호. URL에 안전한 무작위 hex 문자열 사용 |
| `WEB_PORT` | 로컬 Nginx 호스트 포트. 기본값 8080 |
| `DATABASE_URL` | API의 필수 PostgreSQL 연결 값. 런타임 역할 사용 |
| `MIGRATION_DATABASE_URL` | `cloud-api migrate` 명령의 필수 PostgreSQL 연결 값. 별도 마이그레이션 역할 사용 |
| `CLOUD_API_PROXY` | Vite 로컬 프록시 주소. 기본값 `http://127.0.0.1:8080` |
| `API_BIND` | API 소켓 주소. 기본값 `127.0.0.1:3000`, 컨테이너 기본값 `0.0.0.0:3000` |

시험·운영에서는 플랫폼의 비밀 설정으로 별도 DB 연결 값을 제공하고, 배포 전 마이그레이션 역할로 `cloud-api migrate`를 실행합니다. 런타임 역할에는 `public._sqlx_migrations` 조회와 앱에 필요한 권한만 부여합니다. 개발용 Compose는 격리된 개발 DB에서만 동작하도록 구성합니다.
