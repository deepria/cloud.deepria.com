# Deepria Cloud

`cloud.deepria.com`의 Rust API와 Vue 웹 개발환경입니다. 승인된 GitHub 계정으로 로그인한 뒤 내 메모를 작성하고 편집할 수 있습니다.

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

인증까지 Vite에서 시험하려면 API의 `PUBLIC_ORIGIN`과 개발용 GitHub OAuth App의 callback URL을 Vite 주소에 맞춥니다. 예를 들어 `http://127.0.0.1:5173/api/auth/github/callback`을 사용합니다.

아래 검사는 프로젝트 루트에서 실행합니다.

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
(cd apps/web && npm ci && npm run lint && npm run typecheck && npm test && npm run build)
```

## 로컬 GitHub 로그인과 메모

로컬 개발 전용 GitHub OAuth App의 callback URL은 `http://127.0.0.1:8080/api/auth/github/callback`입니다. 로컬 포트를 변경하면 `PUBLIC_ORIGIN`과 callback URL도 같은 주소로 맞춥니다. `.env`의 `GITHUB_OAUTH_CLIENT_ID`와 `GITHUB_OAUTH_CLIENT_SECRET`에 개발용 자격 증명을 설정한 뒤 Compose를 다시 실행합니다.

시험 계정의 GitHub 숫자 ID를 확인해 등록합니다. 해당 GitHub 계정에 대한 승인 근거를 확인한 뒤 아래 명령의 예시 ID와 표시 이름을 바꿉니다.

```sh
docker compose run --rm --no-deps api provision-github 12345678 '시험 계정'
```

로그인은 GitHub 숫자 ID에 매핑한 승인 계정만 허용합니다. 사용자 화면에서 로그인 수단을 연결하거나 해제하는 기능은 제공자를 추가할 때 설계합니다. 운영 HTTPS에서는 host 전용 Secure/HttpOnly 쿠키를 사용합니다. 로컬 HTTP는 루프백 주소에 한해 허용합니다. GitHub 토큰은 프로필 ID 확인에 사용하며 보관하지 않습니다.

메모 편집은 800ms 후 자동 저장됩니다. 새 메모는 처음에 저장 버튼으로 생성합니다. 편집이 다른 곳에서 저장됐다면 서버 버전과 브라우저 초안 중 선택할 수 있습니다. 저장이 끊기면 브라우저에 계정별 초안을 유지합니다.

Rust 통합 시험은 빈 PostgreSQL 데이터베이스 `cloud_test`를 사용합니다. 시험 과정에서 스키마와 가상 계정·메모를 생성합니다. 모의 GitHub 서버가 코드 교환과 PKCE를 검증하고 실제 PostgreSQL에서 세션과 소유권을 검증합니다.

```sh
TEST_DATABASE_URL=postgresql://LOCAL_TEST_ROLE@127.0.0.1:5432/cloud_test \
  cargo test --locked oauth_sessions_and_memos_postgres -- --ignored
```

## 환경변수

| 설정 | 사용처 |
| --- | --- |
| `LOCAL_DB_PASSWORD` | 로컬 Compose의 DB 비밀번호. URL에 안전한 무작위 hex 문자열 사용 |
| `WEB_PORT` | 로컬 Nginx 호스트 포트. 기본값 8080 |
| `DATABASE_URL` | API의 필수 PostgreSQL 연결 값. 런타임 역할 사용 |
| `MIGRATION_DATABASE_URL` | `cloud-api migrate` 명령의 필수 PostgreSQL 연결 값. 별도 마이그레이션 역할 사용 |
| `PUBLIC_ORIGIN` | 브라우저 주소. 끝 슬래시 없이 지정. Compose 기본값은 WEB_PORT를 반영 |
| `GITHUB_OAUTH_CLIENT_ID`, `GITHUB_OAUTH_CLIENT_SECRET` | 이 환경 전용 GitHub OAuth App. HTTPS 시작 시 둘 다 필요 |
| `CLOUD_API_PROXY` | Vite 로컬 프록시 주소. 기본값 `http://127.0.0.1:8080` |
| `API_BIND` | API 소켓 주소. 기본값 `127.0.0.1:3000`, 컨테이너 기본값 `0.0.0.0:3000` |

시험·운영에서는 플랫폼의 비밀 설정으로 별도 DB 연결 값을 제공하고, 배포 전 마이그레이션 역할로 `cloud-api migrate`를 실행합니다. 런타임 역할에는 `public._sqlx_migrations` 조회, `app` 스키마 사용과 로그인·메모 처리에 필요한 테이블 권한만 부여합니다. 계정 등록 명령은 승인된 운영자만 실행하고, 필요 시 해당 명령에 계정 테이블 쓰기 권한이 있는 운영자 전용 연결 값을 사용합니다. 개발용 Compose는 격리된 개발 DB에서만 동작하도록 구성합니다.
