import { expect, test } from '@playwright/test'
import { existsSync, readFileSync, readdirSync } from 'node:fs'
import { resolve } from 'node:path'
import ts from 'typescript'

type ShellTarget = 'user-shell' | 'admin-shell'

interface FrontendManifest {
  target: ShellTarget
  routes: Array<{ path: string }>
}

const repositoryRoot = resolve(import.meta.dirname, '../../../../..')

test('signed frontend manifests never overlap a Shell-owned business route', () => {
  const shellRoutes: Record<ShellTarget, string[]> = {
    'user-shell': literalRoutePaths('services/gateway/frontend/src/router/index.ts'),
    'admin-shell': literalRoutePaths('manager/web/src/main.ts'),
  }
  const manifests = frontendManifests(resolve(repositoryRoot, 'services'))
  expect(manifests.length).toBeGreaterThan(0)

  for (const absolute of manifests) {
    const relative = absolute.slice(repositoryRoot.length + 1)
    const manifest = JSON.parse(readFileSync(absolute, 'utf8')) as FrontendManifest
    for (const contribution of manifest.routes) {
      const conflicts = shellRoutes[manifest.target].filter(
        (owned) => owned === contribution.path || owned.startsWith(`${contribution.path}/`),
      )
      expect(conflicts, `${relative} must exclusively own ${contribution.path}`).toEqual([])
    }
  }
})

function frontendManifests(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .flatMap((entry) => {
      const frontend = resolve(directory, entry.name, 'frontend')
      return existsSync(frontend) ? manifestsBelow(frontend) : []
    })
    .sort()
}

function manifestsBelow(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true })
    .flatMap((entry) => {
      const path = resolve(directory, entry.name)
      if (entry.isDirectory()) return manifestsBelow(path)
      return entry.isFile() && entry.name === 'manifest.json' ? [path] : []
    })
}

function literalRoutePaths(relative: string): string[] {
  const absolute = resolve(repositoryRoot, relative)
  const source = ts.createSourceFile(
    absolute,
    readFileSync(absolute, 'utf8'),
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TS,
  )
  const paths: string[] = []
  const visit = (node: ts.Node): void => {
    if (
      ts.isPropertyAssignment(node)
      && ((ts.isIdentifier(node.name) && node.name.text === 'path')
        || (ts.isStringLiteral(node.name) && node.name.text === 'path'))
      && ts.isStringLiteral(node.initializer)
    ) {
      const path = node.initializer.text
      paths.push(path.startsWith('/') ? path : `/${path}`)
    }
    ts.forEachChild(node, visit)
  }
  visit(source)
  return paths
}
