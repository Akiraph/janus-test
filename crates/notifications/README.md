# Notifications

`janus-notifications` owns deployment-owner notification channel configuration
and the outbound webhook adapters. It supports generic JSON webhooks and
OneBot-compatible QQBot HTTP endpoints without importing a bot SDK.

The capability stores channel credentials encrypted with the deployment master
key and exposes only endpoint metadata and a secret-presence flag. Application
workflows decide which committed public events are worth delivering; this
crate only validates channel configuration and performs delivery.
