# web-search-connector Specification

## Purpose
apb ships a web-search connector, so a playbook can search the web through the connector layer like any other connector call.

## Requirements

### Requirement: The web-search connector
apb SHALL ship an official `web-search` connector over the Brave Search API with `verify` and `search`, authenticated by the `X-Subscription-Token` header, returning titles, URLs, descriptions and whether more results exist, and fetching no pages.

#### Scenario: A fresh search
- **WHEN** an agent searches with `freshness: pw` and `count: 5`
- **THEN** the request carries those parameters and the result is projected to the result list
