# Transport Foundations

This directory contains the public transport helpers: fetch requests, error handling, pagination, event cursors, and thin wrappers around OpenAPI-generated types. It gives features a stable request boundary without owning business state or deciding how a page advances a workflow.

Before sharing state here, verify that it is truly transport infrastructure. Otherwise keep it in the owning feature so `lib` does not become a frontend service locator.
