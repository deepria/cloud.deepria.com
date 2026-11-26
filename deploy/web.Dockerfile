FROM node:24.22.3-bookworm-slim@sha256:873f586f3794f52281bd039bf02c048a0be08fde619338f9941bad1eb6b34530 AS build
WORKDIR /src
COPY apps/web/package.json apps/web/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm npm ci --no-audit --no-fund
COPY apps/web ./
RUN npm run build

FROM nginx/nginx-unprivileged:stable-alpine@sha256:637c35e46af7bc0dd3d3bcf7162e004e13ab3b370fde7cd416e2ac410b5a985a
COPY deploy/nginx.conf /etc/nginx/conf.d/default.conf
COPY --from=build /src/dist/ /usr/share/nginx/html/
USER 101:101
EXPOSE 8080
