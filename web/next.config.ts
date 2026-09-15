import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  output: "export",
  trailingSlash: true,
  images: { unoptimized: true },
  basePath: process.env.NEXT_PUBLIC_BASE_PATH ?? "",
  assetPrefix: process.env.NEXT_PUBLIC_BASE_PATH ?? undefined,
};

export default nextConfig;
