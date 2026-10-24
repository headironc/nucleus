import { writeFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { fileURLToPath } from "node:url";
import { verifyRelease } from "./release.mjs";

export async function assetLocations(root: string) {
  if (!/^\/[a-zA-Z0-9_./-]+$/.test(root))
    throw new Error("Release path must be an absolute path without whitespace");
  const manifest = await verifyRelease(root);
  return (
    manifest.resources
      .map(
        (item) => `location = ${item.path} {
    alias ${root}/build/client/${item.file};
    types {}
    default_type ${item.mediaType};
    add_header Cache-Control "public, max-age=31536000, immutable";
    add_header X-Content-Type-Options nosniff always;
    add_header Referrer-Policy no-referrer always;
    error_page 404 = @halo_asset_missing;
}`,
      )
      .join("\n") +
    `
location /assets/ { return 404; }
location @halo_asset_missing { return 404; }
`
  );
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const root = resolve(process.argv[2] ?? ".");
  await writeFile(
    join(root, "build/nginx-assets.conf"),
    await assetLocations(root),
  );
}
