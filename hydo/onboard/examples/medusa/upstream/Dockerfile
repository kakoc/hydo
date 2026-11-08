# A plain Medusa backend for the reproduction: medusa-starter-default at the commit that pins
# Medusa 2.18.0, with its @medusajs/* versions replaced by MEDUSA_VERSION (so the same file builds
# 2.18.0 and the newest release), this folder's medusa-config.ts (Redis event bus, workflow engine
# and locking, admin dashboard off; no Stripe: the built-in system payment provider is enough) and
# a seed script. Built for production as Medusa's deployment guide says (`medusa build`).
FROM node:22-alpine AS build
RUN apk add --no-cache git
ARG STARTER_COMMIT=9565d9d5487e470137c63824fe58c59a2098bde8
ARG MEDUSA_VERSION=2.18.0
WORKDIR /src
RUN git clone -q https://github.com/medusajs/medusa-starter-default . \
 && git checkout -q "$STARTER_COMMIT" \
 && rm -rf .git yarn.lock .yarnrc.yml \
 && sed -i "s/\"2\.18\.0\"/\"$MEDUSA_VERSION\"/g" package.json \
 && npm install --no-audit --no-fund
COPY medusa-config.ts ./medusa-config.ts
COPY seed.ts ./src/scripts/repro-seed.ts
RUN npx medusa build

FROM node:22-alpine
WORKDIR /server
COPY --from=build /src/.medusa/server/ ./
RUN npm install --omit=dev --no-audit --no-fund && npm cache clean --force
ENV NODE_ENV=production MEDUSA_DISABLE_TELEMETRY=true
EXPOSE 9000
CMD ["npx", "medusa", "start"]
