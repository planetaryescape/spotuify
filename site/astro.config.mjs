import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import { readdirSync } from 'node:fs';

const cliReferenceDir = new URL('./src/content/docs/reference/cli/', import.meta.url);
const cliCommandItems = readdirSync(cliReferenceDir)
  .filter((entry) => entry.endsWith('.md') && !['index.md', 'concepts.md'].includes(entry))
  .map((entry) => entry.replace(/\.md$/, ''))
  .sort((a, b) => a.localeCompare(b))
  .map((slug) => ({ label: slug, slug: `reference/cli/${slug}` }));

export default defineConfig({
  site: 'https://spotuify.dev',
  integrations: [
    starlight({
      title: 'spotuify',
      description:
        'A Spotify daemon with four clients: a keyboard-native TUI, a pipeable CLI, an MCP server for coding agents, and a macOS menubar app.',
      disable404Route: true,
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/planetaryescape/spotuify' },
      ],
      customCss: ['./src/styles/custom.css'],
      head: [
        { tag: 'meta', attrs: { name: 'theme-color', content: '#10130f' } },
        {
          // Default first-time visitors to dark; the theme picker still works
          // and any explicit choice is respected on later visits.
          tag: 'script',
          content:
            "try{if(!localStorage.getItem('starlight-theme')){localStorage.setItem('starlight-theme','dark');document.documentElement.dataset.theme='dark'}}catch(e){}",
        },
        { tag: 'link', attrs: { rel: 'preconnect', href: 'https://fonts.googleapis.com' } },
        { tag: 'link', attrs: { rel: 'preconnect', href: 'https://fonts.gstatic.com', crossorigin: true } },
        {
          tag: 'link',
          attrs: {
            rel: 'stylesheet',
            href: 'https://fonts.googleapis.com/css2?family=Bricolage+Grotesque:opsz,wdth,wght@10..48,75..100,300..800&family=Sometype+Mono:wght@400;500;600;700&display=swap',
          },
        },
      ],
      sidebar: [
        {
          label: 'Start Here',
          items: [
            { label: 'Install', slug: 'getting-started/install' },
            { label: 'Quick Start', slug: 'getting-started/quick-start' },
            { label: 'First Run', slug: 'getting-started/first-run' },
          ],
        },
        {
          label: 'Daily Use',
          items: [
            { label: 'Terminal Control', slug: 'guides/terminal-control' },
            { label: 'Search and Play', slug: 'guides/search-and-play' },
            { label: 'Queue and Playlists', slug: 'guides/queue-and-playlists' },
            { label: 'Browse Artists', slug: 'guides/browse-artists' },
            { label: 'Cache, Search, Sync', slug: 'guides/cache-search-sync' },
            { label: 'Analytics and Hooks', slug: 'guides/analytics-hooks' },
            { label: 'Recipes', slug: 'guides/recipes' },
          ],
        },
        {
          label: 'Architecture',
          items: [
            { label: 'Player and Daemon', slug: 'guides/player-and-daemon' },
            { label: 'Architecture', slug: 'guides/architecture' },
            { label: 'Agent Skill and MCP', slug: 'guides/agents-and-mcp' },
            { label: 'Implementation Roadmap', slug: 'guides/roadmap' },
            { label: 'Research Notes', slug: 'guides/research' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'Config', slug: 'reference/config' },
            { label: 'JSON Output', slug: 'reference/json-output' },
            { label: 'IPC Protocol', slug: 'reference/ipc' },
            { label: 'TUI', slug: 'reference/tui' },
            { label: 'Keybindings', slug: 'reference/keybindings' },
            { label: 'Troubleshooting', slug: 'reference/troubleshooting' },
          ],
        },
        {
          label: 'CLI Reference',
          collapsed: true,
          items: [
            { label: 'Overview', slug: 'reference/cli' },
            { label: 'Concepts', slug: 'reference/cli/concepts' },
            ...cliCommandItems,
          ],
        },
      ],
    }),
  ],
});
