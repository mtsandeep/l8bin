import fs from "fs";

const data = JSON.parse(fs.readFileSync("src/data/products.json", "utf8"));

// Replace niche products with common ones that have easy-to-find Unsplash images
const replacements = {
  79: {
    name: "Oster Belgian Waffle Maker",
    price: 49.99,
    originalPrice: 64.99,
    rating: 4.6,
    reviews: 3847,
    stock: 62,
    category: "Food & Kitchen",
    description: "Makes perfect Belgian waffles in minutes with adjustable temperature control. The non-stick plates ensure easy cleanup and the indicator light tells you when it's ready to bake. A breakfast staple for the whole family.",
    image: "https://images.unsplash.com/photo-1568051243851-f9b136146e97?w=400&h=400&fit=crop&auto=format&q=80",
    tags: ["waffle", "breakfast", "kitchen", "appliance"],
    sku: "FOO-000079",
    weight: "1.4 kg",
    dimensions: "28x25x10 cm",
  },
  80: {
    name: "OXO Good Grips Wooden Spoon Set",
    price: 19.99,
    originalPrice: 24.99,
    rating: 4.8,
    reviews: 2156,
    stock: 140,
    category: "Food & Kitchen",
    description: "Set of 3 solid beechwood spoons in different sizes for stirring, mixing, and serving. The comfortable handles stay cool during cooking and the natural wood won't scratch non-stick cookware.",
    image: "https://images.unsplash.com/photo-1556909114-f9e436f724b2?w=400&h=400&fit=crop&auto=format&q=80",
    tags: ["utensils", "wooden", "cooking", "kitchen"],
    sku: "FOO-000080",
    weight: "0.3 kg",
    dimensions: "30x7x3 cm",
  },
  96: {
    name: "Stanley Classic Vacuum Insulated Bottle",
    price: 35.00,
    originalPrice: 45.00,
    rating: 4.9,
    reviews: 8432,
    stock: 55,
    category: "Travel & Outdoors",
    description: "Iconic 1-quart stainless steel vacuum bottle that keeps drinks cold for 11 hours or hot for 7 hours. Features a leak-proof lid and built-in cup. The hammertone green finish looks great and hides wear.",
    image: "https://images.unsplash.com/photo-1602143407151-7111542de6e8?w=400&h=400&fit=crop&auto=format&q=80",
    tags: ["bottle", "insulated", "stainless", "outdoor"],
    sku: "TRO-000096",
    weight: "0.4 kg",
    dimensions: "34x10x10 cm",
  },
};

for (const p of data) {
  if (replacements[p.id]) {
    Object.assign(p, replacements[p.id]);
    console.log(`#${p.id} ${p.name}`);
    console.log(`  ${p.image.substring(0, 70)}`);
  }
}

fs.writeFileSync("src/data/products.json", JSON.stringify(data, null, 2) + "\n");
console.log("\nReplaced 3 products");
