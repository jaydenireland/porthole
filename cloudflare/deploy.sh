#!/bin/sh
# Builds dist/ from ../web and publishes Porthole to Cloudflare.
#
#   ./deploy.sh             build and publish
#   ./deploy.sh --dry-run   build and check the upload without publishing
#
# First time only: see README.md (sign in, and add the wildcard DNS record).
set -eu
cd "$(dirname "$0")"

[ -d node_modules ] || npm install
node build.mjs
npx wrangler deploy "$@"

case " $* " in
  *" --dry-run "*) exit 0 ;;
esac

echo
echo "Published to https://porthole.app"

# Each tailnet site opens on a subdomain, so the wildcard DNS record has to
# exist. Without it the app loads but no site does.
if node -e 'require("dns").promises.lookup("dns-check--h.porthole.app").then(() => process.exit(0), () => process.exit(1))'; then
  echo "Wildcard DNS for *.porthole.app resolves."
  echo "Check the site with https://porthole.app/test/mock.html"
else
  echo
  echo "WARNING: subdomains of porthole.app do not resolve, so no tailnet site"
  echo "will open. Add the wildcard DNS record described in README.md:"
  echo "  type AAAA, name *, address 100::, proxied"
  echo "A resolver that already looked a subdomain up can keep answering"
  echo "\"not found\" for up to 30 minutes after you add it."
fi
