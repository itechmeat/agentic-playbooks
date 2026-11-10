# web-search-connector Specification

## Purpose
TBD - created by archiving change google-connectors. Update Purpose after archive.

## Requirements

### Requirement: The web-search connector
apb SHALL ship an official `web-search` connector over the Brave Search API with `verify` and `search`, authenticated by the `X-Subscription-Token` header, returning titles, URLs, descriptions and whether more results exist, and fetching no pages.

#### Scenario: A fresh search
- **WHEN** an agent searches with `freshness: pw` and `count: 5`
- **THEN** the request carries those parameters and the result is projected to the result list
