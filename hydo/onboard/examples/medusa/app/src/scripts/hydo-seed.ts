// The test's store, made with the same Medusa workflows as the starter's seed.ts, smaller: one
// region (EUR) paid through Stripe, one warehouse, one shipping option, a publishable API key,
// and one product with one variant whose stock is scarce (SHOP_STOCK units, default 5), so that
// concurrent checkouts compete for the last units. Writes what the test client needs to
// $SHOP_SEED_OUT (default /data/shop/seed.json). Run once: `medusa exec ./src/scripts/hydo-seed.js`.
import { ExecArgs } from "@medusajs/framework/types";
import { ContainerRegistrationKeys, Modules, ProductStatus } from "@medusajs/framework/utils";
import {
  createApiKeysWorkflow,
  createInventoryLevelsWorkflow,
  createProductsWorkflow,
  createRegionsWorkflow,
  createSalesChannelsWorkflow,
  createShippingOptionsWorkflow,
  createShippingProfilesWorkflow,
  createStockLocationsWorkflow,
  linkSalesChannelsToApiKeyWorkflow,
  linkSalesChannelsToStockLocationWorkflow,
  updateStoresWorkflow,
} from "@medusajs/medusa/core-flows";
import * as fs from "fs";
import * as path from "path";

export default async function seed({ container }: ExecArgs) {
  const out = process.env.SHOP_SEED_OUT || "/data/shop/seed.json";
  const stock = Number(process.env.SHOP_STOCK || "5");
  if (fs.existsSync(out)) {
    console.log(`hydo-seed: ${out} exists, nothing to do`);
    return;
  }
  const link = container.resolve(ContainerRegistrationKeys.LINK);
  const query = container.resolve(ContainerRegistrationKeys.QUERY);
  const fulfillment = container.resolve(Modules.FULFILLMENT);
  const storeModule = container.resolve(Modules.STORE);
  const [store] = await storeModule.listStores();

  const { result: [channel] } = await createSalesChannelsWorkflow(container).run({
    input: { salesChannelsData: [{ name: "Webshop" }] },
  });
  await updateStoresWorkflow(container).run({
    input: {
      selector: { id: store.id },
      update: {
        supported_currencies: [{ currency_code: "eur", is_default: true }],
        default_sales_channel_id: channel.id,
      },
    },
  });
  const { result: [region] } = await createRegionsWorkflow(container).run({
    input: {
      regions: [{ name: "Europe", currency_code: "eur", countries: ["de", "dk"], payment_providers: ["pp_stripe_stripe"] }],
    },
  });
  const { result: [location] } = await createStockLocationsWorkflow(container).run({
    input: { locations: [{ name: "Warehouse", address: { city: "Copenhagen", country_code: "DK", address_1: "" } }] },
  });
  await updateStoresWorkflow(container).run({
    input: { selector: { id: store.id }, update: { default_location_id: location.id } },
  });
  await link.create({
    [Modules.STOCK_LOCATION]: { stock_location_id: location.id },
    [Modules.FULFILLMENT]: { fulfillment_provider_id: "manual_manual" },
  });
  const { result: [profile] } = await createShippingProfilesWorkflow(container).run({
    input: { data: [{ name: "Default", type: "default" }] },
  });
  const fset = await fulfillment.createFulfillmentSets({
    name: "Warehouse delivery",
    type: "shipping",
    service_zones: [{ name: "Europe", geo_zones: [{ country_code: "de", type: "country" }, { country_code: "dk", type: "country" }] }],
  });
  await link.create({
    [Modules.STOCK_LOCATION]: { stock_location_id: location.id },
    [Modules.FULFILLMENT]: { fulfillment_set_id: fset.id },
  });
  const { result: [option] } = await createShippingOptionsWorkflow(container).run({
    input: [{
      name: "Standard",
      price_type: "flat",
      provider_id: "manual_manual",
      service_zone_id: fset.service_zones[0].id,
      shipping_profile_id: profile.id,
      type: { label: "Standard", description: "2-3 days", code: "standard" },
      prices: [{ currency_code: "eur", amount: 5 }, { region_id: region.id, amount: 5 }],
      rules: [
        { attribute: "enabled_in_store", value: "true", operator: "eq" },
        { attribute: "is_return", value: "false", operator: "eq" },
      ],
    }],
  });
  await linkSalesChannelsToStockLocationWorkflow(container).run({ input: { id: location.id, add: [channel.id] } });
  const { result: [key] } = await createApiKeysWorkflow(container).run({
    input: { api_keys: [{ title: "Webshop", type: "publishable", created_by: "" }] },
  });
  await linkSalesChannelsToApiKeyWorkflow(container).run({ input: { id: key.id, add: [channel.id] } });
  const { result: [product] } = await createProductsWorkflow(container).run({
    input: {
      products: [{
        title: "Limited Hoodie",
        handle: "limited-hoodie",
        status: ProductStatus.PUBLISHED,
        shipping_profile_id: profile.id,
        options: [{ title: "Size", values: ["M"] }],
        variants: [{ title: "M", sku: "HOODIE-M", manage_inventory: true, options: { Size: "M" }, prices: [{ amount: 40, currency_code: "eur" }] }],
        sales_channels: [{ id: channel.id }],
      }],
    },
  });
  const { data: items } = await query.graph({ entity: "inventory_item", fields: ["id", "sku"] });
  const item = items.find((i: any) => i.sku === "HOODIE-M");
  if (!item) throw new Error("hydo-seed: no inventory item for HOODIE-M");
  await createInventoryLevelsWorkflow(container).run({
    input: { inventory_levels: [{ location_id: location.id, stocked_quantity: stock, inventory_item_id: item.id }] },
  });
  const seed = {
    publishable_key: key.token,
    region_id: region.id,
    variant_id: product.variants[0].id,
    shipping_option_id: option.id,
    inventory_item_id: item.id,
    location_id: location.id,
    stock,
  };
  fs.mkdirSync(path.dirname(out), { recursive: true });
  fs.writeFileSync(out + ".tmp", JSON.stringify(seed) + "\n");
  fs.renameSync(out + ".tmp", out);
  console.log(`hydo-seed: wrote ${out}`);
}
