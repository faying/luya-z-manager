import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  output: "export",
  // *.dev.tsx pages (the /dev-kit component sheet) exist only in `next dev`
  // and never reach web/out.
  pageExtensions:
    process.env.NODE_ENV === "development" ? ["dev.tsx", "tsx", "ts", "jsx", "js"] : ["tsx", "ts", "jsx", "js"],
  trailingSlash: true,
  images: { unoptimized: true },
  // Import only the icons used (keeps dev compiles fast; the build tree-shakes anyway).
  experimental: { optimizePackageImports: ["@phosphor-icons/react"] },
  basePath: process.env.NEXT_PUBLIC_BASE_PATH ?? "",
  assetPrefix: process.env.NEXT_PUBLIC_BASE_PATH ?? undefined,
};

export default nextConfig;
