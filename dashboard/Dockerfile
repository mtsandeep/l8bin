# Build stage
FROM node:24-alpine AS builder
WORKDIR /app
RUN npm install -g pnpm
COPY dashboard/package.json dashboard/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY dashboard/ .
RUN pnpm build

# Runtime stage
FROM nginx:alpine
COPY --from=builder /app/dist /usr/share/nginx/html
EXPOSE 80
