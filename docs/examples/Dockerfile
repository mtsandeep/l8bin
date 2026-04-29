# LiteBin-compatible Dockerfile
# A sample multi-stage Node.js app showing standard Dockerfile usage.
#
# LiteBin detects a Dockerfile in your project root and uses it for `l8b ship`.
# If no Dockerfile is found, Railpack is used as a fallback builder.
#
# There are no special constraints — standard Dockerfiles work as-is.

FROM node:24-alpine AS builder
WORKDIR /app
COPY package*.json ./
RUN npm ci --only=production

FROM node:24-alpine
WORKDIR /app
COPY --from=builder /app/node_modules ./node_modules
COPY . .
EXPOSE 3000
CMD ["node", "server.js"]
