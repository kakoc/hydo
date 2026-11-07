// The starter's medusa-config.ts (medusa-starter-default) with what a production deployment adds:
// Redis for the event bus, the workflow engine and locking (Medusa's production modules), the
// Stripe payment provider, the admin dashboard off (an API-only backend), and PostgreSQL without
// SSL (Medusa's Docker guide).
import { loadEnv, defineConfig } from '@medusajs/framework/utils'

loadEnv(process.env.NODE_ENV || 'development', process.cwd())

const redisUrl = process.env.REDIS_URL

module.exports = defineConfig({
  projectConfig: {
    databaseUrl: process.env.DATABASE_URL,
    databaseDriverOptions: { ssl: false, sslmode: "disable" },
    redisUrl,
    http: {
      storeCors: process.env.STORE_CORS!,
      adminCors: process.env.ADMIN_CORS!,
      authCors: process.env.AUTH_CORS!,
      jwtSecret: process.env.JWT_SECRET || "supersecret",
      cookieSecret: process.env.COOKIE_SECRET || "supersecret",
    }
  },
  admin: { disable: true },
  modules: [
    { resolve: "@medusajs/medusa/event-bus-redis", options: { redisUrl } },
    { resolve: "@medusajs/medusa/workflow-engine-redis", options: { redis: { url: redisUrl } } },
    {
      resolve: "@medusajs/medusa/locking",
      options: {
        providers: [
          { resolve: "@medusajs/medusa/locking-redis", id: "locking-redis", is_default: true, options: { redisUrl } },
        ],
      },
    },
    {
      resolve: "@medusajs/medusa/payment",
      options: {
        providers: [
          {
            resolve: "@medusajs/medusa/payment-stripe",
            id: "stripe",
            options: {
              apiKey: process.env.STRIPE_API_KEY,
              webhookSecret: process.env.STRIPE_WEBHOOK_SECRET,
            },
          },
        ],
      },
    },
  ],
})
