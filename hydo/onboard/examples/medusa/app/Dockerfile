# The shop's backend image: Medusa's starter (medusa-starter-default, cloned into starter/ by
# build-app.sh at a pinned commit) with this example's medusa-config.ts and seed script, built for
# production as Medusa's deployment guide says (`medusa build`, then the built server's production
# dependencies), API only.
FROM node:22-alpine AS build
WORKDIR /src
COPY starter/ ./
COPY medusa-config.ts ./medusa-config.ts
COPY src/scripts/hydo-seed.ts ./src/scripts/hydo-seed.ts
RUN corepack enable && yarn install --immutable && yarn medusa build

FROM node:22-alpine
WORKDIR /server
COPY --from=build /src/.medusa/server/ ./
RUN npm install --omit=dev --no-audit --no-fund && npm cache clean --force
ENV NODE_ENV=production MEDUSA_DISABLE_TELEMETRY=true
EXPOSE 9000
CMD ["npx", "medusa", "start"]
