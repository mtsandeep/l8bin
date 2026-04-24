FROM node:22-alpine AS build
WORKDIR /app
COPY package*.json ./
RUN npm install
COPY . .
RUN npm run build

FROM node:22-alpine
WORKDIR /app
COPY --from=build /app/dist ./dist
COPY --from=build /app/node_modules ./node_modules
COPY --from=build /app/vite.config.js ./
EXPOSE 80
CMD ["npx", "vite", "preview", "--host", "0.0.0.0", "--port", "80"]
