// scripts/check-images.mjs
// Run: node scripts/check-images.mjs
// Optional: UNSPLASH_ACCESS_KEY=xxx node scripts/check-images.mjs (auto-fixes broken URLs)

import fs from "fs";
import path from "path";

const PRODUCTS_FILE = path.resolve("src/data/products.json");
const data = JSON.parse(fs.readFileSync(PRODUCTS_FILE, "utf8"));

const UNSPLASH_KEY = process.env.UNSPLASH_ACCESS_KEY;

async function checkUrl(url) {
  try {
    const res = await fetch(url, { method: "HEAD", redirect: "follow" });
    return res.ok;
  } catch {
    return false;
  }
}

async function searchUnsplash(query) {
  if (!UNSPLASH_KEY) return null;
  try {
    const res = await fetch(
      `https://api.unsplash.com/search/photos?query=${encodeURIComponent(query)}&per_page=3`,
      { headers: { Authorization: `Client-ID ${UNSPLASH_KEY}` } }
    );
    if (!res.ok) return null;
    const json = await res.json();
    if (json.results?.length > 0) {
      return `${json.results[0].urls.raw}?w=400&h=400&fit=crop&auto=format&q=80`;
    }
  } catch {}
  return null;
}

function buildSearchQueries(product) {
  const name = product.name;
  const category = product.category;
  const tags = product.tags || [];

  // Extract key words from product name (drop brand names, model numbers, sizes)
  const generic = name
    .replace(/\b(Samsung|Apple|Sony|Nike|Logitech|KitchenAid|Levi's|Patagonia|Nintendo|Calvin Klein|Le Creuset|Cuisinart|Oxo|Smeg|Miyabi|Baratza|All-Clad|Villeroy|Boch|Technivorm|Boos Block|Marcato|Meater|Jetboil|LifeStraw|ENO|NEMO|Black Diamond|Goal Zero|Nikon|Cole Haan|Dr\.? Martens|New Balance|Ray-Ban|Seiko|Oakley|Fjällräven|Osprey|Big Agnes|Salomon|Marmot|Yeti|Petzl|Sea to Summit|REI|Altra|Leatherman|Cabeau|Amazon|Google|Breville|Instant Pot|Dyson|Bose|JBL|Canon|DJI|GoPro|Garmin|Fitbit)\b/gi, "")
    .replace(/\b(Model|Series|Original|Classic|Standard|Premium|Professional|Advanced|Digital|Electric|Personal)\b/gi, "")
    .replace(/\s+/g, " ")
    .trim();

  const queries = [];

  // 1. Full name
  queries.push(name);

  // 2. Category + generic keywords
  if (generic.length > 2) {
    queries.push(`${category} ${generic}`);
  }

  // 3. Just the generic name (dropped brands/model numbers)
  if (generic.length > 2 && generic !== name) {
    queries.push(generic);
  }

  // 4. Category + first meaningful tag
  const meaningfulTag = tags.find(
    (t) => t !== category.toLowerCase() && t.length > 2 && !t.includes("premium") && !t.includes("standard")
  );
  if (meaningfulTag) {
    queries.push(`${category} ${meaningfulTag}`);
  }

  // 5. Fallback: just category + generic term
  const categoryFallbacks = {
    Electronics: "technology gadget",
    Fashion: "clothing apparel",
    "Home & Living": "home decor interior",
    "Food & Kitchen": "kitchen cooking",
    "Travel & Outdoors": "outdoor adventure travel",
  };
  queries.push(categoryFallbacks[category] || category);

  // Deduplicate
  return [...new Set(queries.map((q) => q.toLowerCase().trim()).filter(Boolean))];
}

console.log(`Checking ${data.length} product images...\n`);

const broken = [];
let checked = 0;

// Check in batches of 5 to avoid rate limiting
for (let i = 0; i < data.length; i += 5) {
  const batch = data.slice(i, i + 5);
  const results = await Promise.all(
    batch.map(async (p) => {
      const ok = await checkUrl(p.image);
      checked++;
      process.stdout.write(`\r[${checked}/${data.length}]`);
      return { product: p, ok };
    })
  );

  for (const { product, ok } of results) {
    if (!ok) {
      broken.push(product);
    }
  }
}

console.log(`\n\nResults: ${data.length - broken.length} OK, ${broken.length} broken\n`);

if (broken.length === 0) {
  console.log("All images are working!");
  process.exit(0);
}

if (!UNSPLASH_KEY) {
  console.log("BROKEN IMAGES (set UNSPLASH_ACCESS_KEY to auto-fix):");
  console.log("-".repeat(80));
  for (const p of broken) {
    console.log(`  #${p.id} ${p.name}`);
    console.log(`       ${p.image}`);
    console.log();
  }
  process.exit(1);
}

// Auto-fix with Unsplash search — tries multiple queries per product
console.log("Auto-fixing broken images via Unsplash API...\n");
let fixed = 0;

for (const p of broken) {
  const queries = buildSearchQueries(p);
  let newUrl = null;
  let usedQuery = null;

  for (const query of queries) {
    newUrl = await searchUnsplash(query);
    if (newUrl) {
      usedQuery = query;
      break;
    }
  }

  if (newUrl) {
    console.log(`  #${p.id} ${p.name}`);
    console.log(`    OLD: ${p.image}`);
    console.log(`    NEW: ${newUrl}`);
    console.log(`    Query: "${usedQuery}"`);
    p.image = newUrl;
    fixed++;
  } else {
    console.log(`  #${p.id} ${p.name} — NO REPLACEMENT FOUND`);
    console.log(`       ${p.image}`);
    console.log(`       Tried: ${queries.join(", ")}`);
  }
  console.log();
}

if (fixed > 0) {
  fs.writeFileSync(PRODUCTS_FILE, JSON.stringify(data, null, 2) + "\n");
  console.log(`Fixed ${fixed} URLs in products.json`);
}

if (fixed < broken.length) {
  console.log(`${broken.length - fixed} URLs could not be auto-fixed`);
  process.exit(1);
}
