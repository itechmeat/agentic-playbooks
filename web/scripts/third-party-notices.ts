// Prints the third-party notices for every npm package whose code or assets
// end up in web/dist, the frontend apb-server embeds into the apb binary.
//
// The package list is what the production build actually emits, not the
// dependency tree: the script runs the real Vite build (write: false, so
// web/dist is untouched) with one extra plugin that records the modules
// rendered into every output chunk. Tree-shaken packages drop out, build
// tools that put nothing into the bundle never appear. CSS pulled in by
// `@import` in src/**/*.css is compiled inside the Tailwind plugin and never
// becomes a separate module, so those imports are read from the stylesheets
// directly. Vite and Rolldown inject small runtime helpers (virtual modules
// such as "\0vite/preload-helper.js"), so they are listed when present.
//
// Run through scripts/third-party-notices.sh from the repository root.
// Output is deterministic: packages sorted by name, line endings normalized,
// no local paths.
import { readFileSync, readdirSync, statSync } from 'node:fs'
import path from 'node:path'
import { build, type Plugin } from 'vite'

const webDir = path.resolve(import.meta.dirname, '..')
process.chdir(webDir)

const LICENSE_FILE = /^(licen[cs]e|copying|notice)(\.(md|txt|markdown))?$/i
// Vite's LICENSE.md appends the licenses of the dependencies bundled into
// Vite itself (the build tool), none of which reach web/dist. Keep only the
// part above that marker.
const BUNDLED_SECTION = /^#+\s*Licenses of bundled dependencies/im

function packageRoot(id: string): string | null {
  const clean = id.replace(/^\0/, '').split('?')[0]
  const idx = clean.lastIndexOf('/node_modules/')
  if (idx < 0) return null
  const rest = clean.slice(idx + '/node_modules/'.length).split('/')
  const name = rest[0].startsWith('@') ? `${rest[0]}/${rest[1]}` : rest[0]
  return clean.slice(0, idx) + '/node_modules/' + name
}

function resolvePackage(name: string): string {
  return path.join(webDir, 'node_modules', name)
}

function cssImportPackages(dir: string, out: Set<string>) {
  for (const entry of readdirSync(dir).sort()) {
    const full = path.join(dir, entry)
    if (statSync(full).isDirectory()) {
      cssImportPackages(full, out)
    } else if (entry.endsWith('.css')) {
      const css = readFileSync(full, 'utf8')
      for (const m of css.matchAll(/@(?:import|plugin)\s+(?:url\()?['"]([^'"]+)['"]/g)) {
        const spec = m[1]
        if (spec.startsWith('.') || spec.startsWith('/') || spec.includes(':')) continue
        const parts = spec.split('/')
        out.add(resolvePackage(spec.startsWith('@') ? `${parts[0]}/${parts[1]}` : parts[0]))
      }
    }
  }
}

const roots = new Set<string>()
const collector: Plugin = {
  name: 'apb-third-party-notices',
  generateBundle(_options, bundle) {
    for (const file of Object.values(bundle)) {
      const ids =
        file.type === 'chunk' ? file.moduleIds : ((file as { originalFileNames?: string[] }).originalFileNames ?? [])
      for (const id of ids) {
        const root = packageRoot(path.resolve(webDir, id))
        if (root) roots.add(root)
        else if (id.startsWith('\0vite/')) roots.add(resolvePackage('vite'))
        else if (id.startsWith('\0rolldown/')) roots.add(resolvePackage('rolldown'))
      }
    }
  },
}

await build({
  configFile: path.join(webDir, 'vite.config.ts'),
  logLevel: 'silent',
  build: { write: false },
  plugins: [collector],
})
cssImportPackages(path.join(webDir, 'src'), roots)

type Pkg = { name: string; version: string; license: string; repo: string; texts: string[] }

function repoUrl(repository: unknown): string {
  const raw = typeof repository === 'string' ? repository : (repository as { url?: string } | undefined)?.url
  if (!raw) return ''
  return raw
    .replace(/^git\+/, '')
    .replace(/\.git$/, '')
    .replace(/^git:\/\//, 'https://')
    .replace(/^github:/, 'https://github.com/')
    .replace(/^(?!https?:)([\w.-]+\/[\w.-]+)$/, 'https://github.com/$1')
}

function normalize(text: string): string {
  return text
    .replace(/\r\n?/g, '\n')
    .split('\n')
    .map((l) => l.replace(/\s+$/, ''))
    .join('\n')
    .trim()
}

// A package without a `license` field is classified from its license file.
// Only the MIT text is recognized; anything else stays UNKNOWN so it shows up
// for review.
function licenseFromText(texts: string[]): string {
  return texts.some((t) => t.includes('Permission is hereby granted, free of charge')) ? 'MIT' : 'UNKNOWN'
}

const pkgs: Pkg[] = []
const seen = new Set<string>()
for (const root of roots) {
  const meta = JSON.parse(readFileSync(path.join(root, 'package.json'), 'utf8'))
  // The same name and version can sit in several nested node_modules.
  const key = `${meta.name}@${meta.version}`
  if (seen.has(key)) continue
  seen.add(key)
  const license =
    typeof meta.license === 'string'
      ? meta.license
      : (meta.license?.type ?? (meta.licenses ?? []).map((l: { type: string }) => l.type).join(' OR '))
  const texts = readdirSync(root)
    .filter((f) => LICENSE_FILE.test(f) && statSync(path.join(root, f)).isFile())
    .sort()
    .map((f) => {
      let text = readFileSync(path.join(root, f), 'utf8')
      const cut = text.search(BUNDLED_SECTION)
      if (cut > 0) text = text.slice(0, cut)
      return normalize(text)
    })
  pkgs.push({ name: meta.name, version: meta.version, license: license || licenseFromText(texts), repo: repoUrl(meta.repository), texts })
}
const byText = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0)
pkgs.sort((a, b) => byText(a.name, b.name) || byText(a.version, b.version))

const rule = '-'.repeat(80)
const lines: string[] = []
lines.push(`The web frontend embedded in apb includes ${pkgs.length} npm packages.`, '')
const counts = new Map<string, number>()
for (const p of pkgs) counts.set(p.license, (counts.get(p.license) ?? 0) + 1)
for (const [lic, n] of [...counts].sort()) lines.push(`  ${lic}: ${n}`)
lines.push('')
for (const p of pkgs) {
  lines.push(rule, `${p.name} ${p.version}`, `License: ${p.license}`)
  if (p.repo) lines.push(`Source: ${p.repo}`)
  lines.push('')
  if (p.texts.length === 0) {
    lines.push(`The package ships no license file. It is distributed under ${p.license}.`, '')
  }
  for (const t of p.texts) lines.push(t, '')
}
process.stdout.write(lines.join('\n'))
