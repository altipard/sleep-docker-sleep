# Embedded third-party assets

## Swagger UI 5.17.14

- Upstream: https://github.com/swagger-api/swagger-ui
- Package: https://www.npmjs.com/package/swagger-ui-dist/v/5.17.14
- License: Apache-2.0, preserved in `swagger-ui-LICENSE`.
- `swagger-ui.css` SHA-256: `40170f0ee859d17f92131ba707329a88a070e4f66874d11365e9a77d232f6117`
- `swagger-ui-bundle.js` SHA-256: `c2e4a9ef08144839ff47c14202063ecfe4e59e70a4e7154a26bd50d880c88ba1`

These files are embedded unmodified so the admin UI works offline and never executes
third-party CDN JavaScript in the privileged `sds` browser origin.
