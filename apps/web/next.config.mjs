/** @type {import('next').NextConfig} */
const securityHeaders = [
  { key: "X-Content-Type-Options", value: "nosniff" },
  { key: "X-Frame-Options", value: "DENY" },
  { key: "Referrer-Policy", value: "same-origin" },
];
export default {
  output: "standalone",
  poweredByHeader: false,
  // XFlow serves no optimised images. Next 15 would otherwise enable /_next/image and ship sharp (native libvips)
  // in the standalone image, which is attack surface (cf. the AVIF image-optimizer RCE advisory) for no benefit.
  images: { unoptimized: true },
  async headers() {
    return [{ source: "/:path*", headers: securityHeaders }];
  },
};
