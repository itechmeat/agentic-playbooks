## ADDED Requirements

### Requirement: Declared paging
A read-only HTTP GET function MAY declare `paginate` with an items path and either `link_header: true` or a `token_path` and `token_arg`. Paging SHALL be refused on any other function, on an identity or reconcile function, and with `link_header` on a connector whose auth is `path` or `query`.

#### Scenario: Paging a write is refused
- **WHEN** a function that is not read-only declares `paginate`
- **THEN** the manifest does not load

### Requirement: The walk is bounded
A paginated call SHALL fetch pages until no next page exists, `max_pages` (default 5, at most 20) is reached, or 4 MiB were read. The result SHALL hold every page's items in order, `pages` and `more`, and the last page's body around the items.

#### Scenario: Two pages
- **WHEN** the first page names a next page and the second does not
- **THEN** the result holds both pages' items, `pages: 2` and `more: false`

#### Scenario: Stopped at the limit
- **WHEN** every page names a next page and `max_pages` is 5
- **THEN** five requests are made and the result says `more: true`

### Requirement: Next pages stay on the first host
A `Link` next URL SHALL be followed only when its scheme, host and port equal the first request's; otherwise the call SHALL fail `service` without a further request.

#### Scenario: A link to another host
- **WHEN** the `Link` header points at another host
- **THEN** the call fails and only one request was made

### Requirement: A failed page fails the call
An error on any page SHALL fail the whole call with that error, prefixed with the page number from page 2 on.

#### Scenario: Page two fails
- **WHEN** page 2 answers HTTP 500
- **THEN** the call fails `service` with a message that starts with "page 2 of the listing"

### Requirement: One call, one journal event
A paginated call SHALL count once against `max_calls` and SHALL journal one `connector_call` event with `pages` set.

#### Scenario: Budget of one
- **WHEN** a binding has `max_calls: 1` and the read takes three pages
- **THEN** the call succeeds and the journal holds one event with `pages: 3`

### Requirement: Contract tests with scripted responses
A `tests.yaml` HTTP case MAY give `responses` and assert `requests`, `result_contains` or `error_code`; the offline runner SHALL answer the rendered requests with them through the live response path.

#### Scenario: A scripted paging case
- **WHEN** a case scripts two pages for `github.list_issues`
- **THEN** `apb connector test github` checks both request URLs and the merged items

### Requirement: Optional item lists
With `items_optional: true`, a page without the items field SHALL read as an empty page; a present non-array SHALL still fail.

#### Scenario: Nothing matched
- **WHEN** a Gmail search answers `{ resultSizeEstimate: 0 }`
- **THEN** the call succeeds with an empty `messages` list

### Requirement: The byte cap bounds every read
Each page SHALL be read with an allowance of the smaller of the per-body cap and what is left of the 4 MiB total; a page cut by it SHALL fail the call `incomplete_response` with none of its items returned.

#### Scenario: A second page past the total
- **WHEN** page 1 is 3 MiB and page 2 is 2 MiB
- **THEN** the call fails `incomplete_response` and no item of page 2 is returned

### Requirement: Failed walks keep their facts
A failed paginated call SHALL journal the pages attempted and the last service status that arrived, and a failure of a later page SHALL carry its page number whether the request, the read or the shaping failed.

#### Scenario: Page two times out
- **WHEN** page 2 times out
- **THEN** the error message starts with "page 2 of the listing" and the event has `pages: 2`
