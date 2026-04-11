FROM node:24-alpine AS builder
WORKDIR /app
COPY package.json ./
RUN npm install
COPY . .
RUN npm run build

FROM node:24-alpine
WORKDIR /app
COPY package.json ./
RUN npm install --omit=dev
COPY server/ server/
COPY --from=builder /app/dist dist/
EXPOSE 3000
CMD ["node", "server/index.js"]
