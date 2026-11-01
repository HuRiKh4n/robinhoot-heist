// Bundles the wallet selector into one local file, so the page does not depend on a CDN at runtime.
import { build } from 'esbuild';
await build({
  stdin: { contents: "export { NearConnector } from '@hot-labs/near-connect';", resolveDir: process.cwd(), loader: 'js' },
  bundle: true, format: 'esm', platform: 'browser', target: 'es2020', minify: true,
  outfile: '../web/vendor/near-connect.js',
  define: { 'process.env.NODE_ENV': '"production"', global: 'globalThis' },
});
console.log('bundled web/vendor/near-connect.js');
