<script lang="ts">
	/**
	 * System — the store, as it is right now.
	 *
	 * The words are memory_status (health, retention, coverage); the river
	 * shows whichever memories the last action touched:
	 *   durable      project preview: what a CLAUDE.md projection would carry
	 *                (project write, with a path and a confirm);
	 *   dream        maintain dream: replays recorded edges and reviews
	 *                memories on the FSRS curve, strengthening some;
	 *   brief        session_start: what an agent is handed when it opens;
	 *   collect      maintain gc, dry run: what would be collected;
	 *   tools        memory_status tools: every tool this dashboard calls.
	 */
	import '$lib/trace/trace-organ.css';
	import { alive } from '$lib/trace/alive';
	import { onMount } from 'svelte';
	import OrganStage from '$lib/trace/OrganStage.svelte';
	import type { StageWords } from '$lib/trace/TraceStage.svelte';
	import { api } from '$lib/stores/api';
	import { osHref } from '$lib/os-nav';
	import { typeColor } from '$lib/trace/trace-pass';
	import { headline, type TraceMemory } from '$lib/trace/trace-data';
	import { fetchMemories } from '$lib/trace/trace-fetch';
	import MemoryFocus from '$lib/trace/MemoryFocus.svelte';
	import { goto } from '$app/navigation';
	import Dropdown, { type DropdownOption } from '$lib/components/Dropdown.svelte';
	import type { OrcaRouterState, OrcaRouterModelsResponse } from '$types';
	import type { VerifyState } from '$lib/trace/trace-pass';
	import type { TraceLink } from '$lib/trace/trace-data';

	type Lens = 'durable' | 'dream' | 'brief' | 'collect' | 'tools' | 'verifier';
	const LENSES: { key: Lens; label: string }[] = [
		{ key: 'durable', label: 'what lasts' },
		{ key: 'dream', label: 'dream now' },
		{ key: 'brief', label: 'session brief' },
		{ key: 'collect', label: 'collect (dry run)' },
		{ key: 'tools', label: 'every tool' },
		{ key: 'verifier', label: 'verifier' }
	];
	/** Where each tool is live in the dashboard. */
	const TOOL_HOME: Record<string, string> = {
		recall: '/trace',
		memory_status: '/ledger',
		receipt: '/trace',
		causal_walk: '/causal',
		forgotten_lesson: '/causal',
		selftest: '/causal',
		memory: '/trace',
		suppress: '/trace',
		smart_ingest: '/ledger',
		intention: '/ledger',
		vestige_stops: '/guard',
		dedup: '/healing',
		ghostlink: '/healing',
		codebase: '/healing',
		maintain: '/system',
		project: '/system',
		session_start: '/system',
		gather: '/causal'
	};

	interface Health {
		totalMemories?: number;
		averageRetention?: number;
		status?: string;
		dueForReview?: number;
		warnings?: string[];
		newestMemory?: string;
		oldestMemory?: string;
	}

	let lens = $state<Lens>('durable');
	let loading = $state(true);
	let busy = $state(false);
	let error = $state<string | null>(null);
	let titleEl = $state<HTMLElement | null>(null);
	let pickedId = $state<string | null>(null);

	let health = $state<Health | null>(null);
	let retention = $state<{ bucket: string; count: number }[]>([]);
	let below50 = $state<number | null>(null);
	let edges = $state<[string, number][]>([]);

	let river = $state<TraceMemory[]>([]);
	let durableRegion = $state<string | null>(null);
	let projectPath = $state('');
	let projectConfirm = $state(false);
	let projectNote = $state<string | null>(null);
	let dream = $state<{ durationMs?: number; memoriesReplayed?: number; strengthened?: number; edgesConsidered?: number; discovery?: string; reviews: { id: string; rating?: number; retention?: number; stability?: number }[] } | null>(null);
	let brief = $state<string | null>(null);
	let collect = $state<{ candidateCount?: number; processed?: number } | null>(null);
	let tools = $state<{ name: string; description: string }[]>([]);

	// ── The verifier: OrcaRouter is the named provider behind the Stop hook.
	// The key is write-only from here (it is stored in the hook's 0600 env
	// file and never read back), and every model id in the dropdown comes
	// from the live catalog the backend fetches — never from this file.
	let verifier = $state<OrcaRouterState | null>(null);
	let keyDraft = $state('');
	let savingKey = $state(false);
	let keyNote = $state<string | null>(null);
	let catalog = $state<OrcaRouterModelsResponse | null>(null);
	let loadingCatalog = $state(false);
	let catalogError = $state<string | null>(null);
	let catalogNote = $state<string | null>(null);
	let chosenModel = $state('');
	let savingModel = $state(false);

	const modelOptions = $derived<DropdownOption[]>(
		(catalog?.models ?? []).map((m) => ({
			value: m.id,
			label: m.label,
			badge: m.supportsReasoning ? 'reasoning' : undefined
		}))
	);

	async function loadVerifier() {
		try {
			verifier = await api.orcarouter.state();
		} catch (e) {
			catalogError = e instanceof Error ? e.message : String(e);
		}
	}

	/**
	 * Read the catalog the gateway serves this key. A failed read keeps the
	 * last good list on screen and says why; it never invents a model.
	 */
	async function refreshCatalog() {
		loadingCatalog = true;
		catalogError = null;
		catalogNote = null;
		try {
			const next = await api.orcarouter.models();
			catalog = next;
			// A model that disappeared from the catalog must not stay selected:
			// the hook would then call an id the gateway no longer serves.
			const ids = new Set(next.models.map((m) => m.id));
			const preferred = verifier?.model ?? '';
			if (chosenModel && !ids.has(chosenModel)) {
				catalogNote = `${chosenModel} is no longer in the catalog; choose again.`;
				chosenModel = '';
			}
			if (!chosenModel && ids.has(preferred)) chosenModel = preferred;
			if (!chosenModel && ids.has('orcarouter/fusion')) chosenModel = 'orcarouter/fusion';
			if (!chosenModel && next.models.length) chosenModel = next.models[0].id;
		} catch (e) {
			catalogError = e instanceof Error ? e.message : String(e);
		} finally {
			loadingCatalog = false;
		}
	}

	async function saveKey() {
		const draft = keyDraft.trim();
		if (!draft || savingKey) return;
		savingKey = true;
		keyNote = null;
		try {
			const res = await api.orcarouter.storeKey(draft);
			keyDraft = '';
			verifier = await api.orcarouter.state();
			keyNote = res.keyPresent
				? 'Stored. The hook reads it from the env file; it is never shown again.'
				: 'The key was not stored.';
			if (res.keyPresent) await refreshCatalog();
		} catch (e) {
			keyNote = e instanceof Error ? e.message : String(e);
		} finally {
			savingKey = false;
		}
	}

	async function saveModel(id: string) {
		savingModel = true;
		catalogNote = null;
		try {
			await api.orcarouter.chooseModel(id);
			verifier = await api.orcarouter.state();
			catalogNote = `The verifier now calls ${id}.`;
		} catch (e) {
			catalogNote = e instanceof Error ? e.message : String(e);
		} finally {
			savingModel = false;
		}
	}

	const byId = $derived(new Map(river.map((m) => [m.id, m])));
	let focusLinks = $state<TraceLink[]>([]);
	let verify = $state<VerifyState>('idle');
	/** A connected memory: fly to it when it is on this river, else open it in Trace. */
	function focusPick(id: string) {
		if (byId.has(id)) pickedId = id;
		else void goto(osHref('/trace', { q: id }));
	}
	$effect(() => {
		if (!pickedId) {
			focusLinks = [];
			verify = 'idle';
		}
	});
	const picked = $derived(pickedId ? (byId.get(pickedId) ?? null) : null);
	const reviewById = $derived(new Map((dream?.reviews ?? []).map((r) => [r.id, r])));

	async function loadVitals() {
		const [h, r, c] = await Promise.all([
			api.tool<Health>('memory_status', { view: 'health' }),
			api.tool<{ distribution?: { bucket: string; count: number }[]; memoriesBelow50pct?: number }>('memory_status', { view: 'retention' }),
			api.tool<{ edgeCountsByType?: Record<string, number> }>('memory_status', { view: 'coverage' })
		]);
		health = h;
		retention = r.distribution ?? [];
		below50 = r.memoriesBelow50pct ?? null;
		edges = Object.entries(c.edgeCountsByType ?? {}).sort((a, b) => b[1] - a[1]);
	}

	async function run(next: Lens) {
		lens = next;
		pickedId = null;
		busy = true;
		error = null;
		try {
			if (next === 'durable') {
				const res = await api.tool<{ items?: { id: string }[]; region?: string }>('project', { action: 'preview', max_items: 60 });
				durableRegion = res.region ?? null;
				const full = await fetchMemories((res.items ?? []).map((i) => i.id));
				river = [...full.values()];
			} else if (next === 'dream') {
				const res = await api.tool<{
					durationMs?: number;
					memoriesReplayed?: number;
					edgesConsidered?: number;
					stats?: { memories_strengthened?: number };
					discovery?: { detail?: string };
					reviews?: { id: string; rating?: number; retention?: number; stability?: number }[];
				}>('maintain', { action: 'dream' });
				dream = {
					durationMs: res.durationMs,
					memoriesReplayed: res.memoriesReplayed,
					strengthened: res.stats?.memories_strengthened,
					edgesConsidered: res.edgesConsidered,
					discovery: res.discovery?.detail,
					reviews: res.reviews ?? []
				};
				const full = await fetchMemories(dream.reviews.map((r) => r.id));
				// The dream's verdict is the row: how each memory came back when it
				// was replayed on its FSRS curve (4 easy, 3 good, 2 hard, 1 again).
				const RATED: Record<number, string> = { 4: 'came back easily', 3: 'came back', 2: 'came back with effort', 1: 'slipped' };
				const rating = new Map(dream.reviews.map((r) => [r.id, r.rating]));
				river = [...full.values()].map((m) => {
					const r = rating.get(m.id);
					return r && RATED[r] ? { ...m, laneLabel: RATED[r], lane: r } : m;
				});
			} else if (next === 'brief') {
				const res = await api.tool<{ context?: string; codeContext?: { items?: { id: string }[] } }>('session_start', {
					include_intentions: true,
					include_status: true
				});
				brief = res.context ?? null;
				const ids = [...(brief ?? '').matchAll(/\[(mem-[0-9a-f]{16}|[0-9a-f]{8}-[0-9a-f-]{27})\]/g)].map((m) => m[1]);
				const full = ids.length ? await fetchMemories(ids) : new Map();
				river = [...full.values()];
			} else if (next === 'collect') {
				collect = await api.tool<{ candidateCount?: number; processed?: number }>('maintain', { action: 'gc', dry_run: true });
			} else if (next === 'tools') {
				const res = await api.tool<{ tools?: { name: string; description: string }[] }>('memory_status', { view: 'tools' });
				tools = (res.tools ?? []).map((t) => ({ name: t.name, description: t.description }));
			} else {
				if (!verifier) await loadVerifier();
				if (!catalog) await refreshCatalog();
			}
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		} finally {
			busy = false;
		}
	}

	async function writeProjection() {
		if (!projectPath.trim()) return;
		if (!projectConfirm) {
			projectConfirm = true;
			return;
		}
		try {
			const res = await api.tool<{ path?: string; receiptId?: string; itemCount?: number }>('project', {
				action: 'write',
				path: projectPath.trim(),
				confirm: true,
				max_items: 60
			});
			projectNote = `Wrote ${res.itemCount ?? 'the'} memories into ${res.path ?? projectPath} · only the fenced region changed`;
		} catch (e) {
			projectNote = e instanceof Error ? e.message : String(e);
		} finally {
			projectConfirm = false;
		}
	}

	function describe(id: string): StageWords | null {
		const m = byId.get(id);
		if (!m) return null;
		const r = reviewById.get(id);
		return {
			kicker: `${m.type}${r ? ` · reviewed, rating ${r.rating}, stability ${Math.round(r.stability ?? 0)} d` : ''} · ${Math.round(m.retention * 100)}% retained`,
			text: m.content,
			tone: m.type
		};
	}

	const rgb = (t: string) => {
		const [r, g, b] = typeColor(t);
		return `rgb(${Math.round(r * 255)} ${Math.round(g * 255)} ${Math.round(b * 255)})`;
	};
	function when(iso?: string): string {
		if (!iso) return '';
		const d = new Date(iso);
		return Number.isFinite(d.getTime()) ? d.toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' }) : '';
	}

	onMount(async () => {
		try {
			await loadVitals();
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
		}
		loading = false;
		void loadVerifier();
		await run('durable');
	});
</script>

<svelte:head>
	<title>System · Vestige</title>
</svelte:head>

<OrganStage
	memories={river}
	selectedId={pickedId}
	links={focusLinks}
	{verify}
	origin={() => {
		const r = titleEl?.getBoundingClientRect();
		return r ? { x: ((r.left + 160) / innerWidth) * 2 - 1, y: 1 - ((r.top + r.height / 2) / innerHeight) * 2 } : undefined;
	}}
	{describe}
	labelIds={pickedId ? [pickedId] : []}
	onpick={(id) => (pickedId = id)}
	story="system"
	seed="vestige-system-v1"
	axis="order"
/>

<header class="tr-ask" class:tr-quiet={!!pickedId}>
	<p class="tr-eyebrow" use:alive={{ text: 'System', glow: true }}></p>
	<!-- svelte-ignore a11y_missing_content (alive renders the text and its aria-label) -->
	<h1 class="tr-title" bind:this={titleEl} use:alive={{ text: 'The store, as it is right now.', glow: true }}></h1>
	<p class="tr-hint">
		{#each LENSES as l (l.key)}
			<button type="button" class="tr-chip" aria-pressed={lens === l.key} disabled={busy} onclick={() => void run(l.key)}>{l.label}</button>
		{/each}
	</p>
	{#if loading}
		<p class="tr-line">Reading the store's health…</p>
	{:else if health}
		<p class="tr-line">
			<strong use:alive={{ text: String(health.totalMemories?.toLocaleString()), count: true }}></strong> memories, {health.status ?? 'status unknown'},
			average retention {Math.round((health.averageRetention ?? 0) * 100)}%{below50 ? `, ${below50.toLocaleString()} below half` : ''}.
		</p>
		<p class="tr-types">
			{#each retention as b (b.bucket)}<span>{b.bucket} {b.count.toLocaleString()}</span>{/each}
		</p>
		<p class="tr-types">
			{#each edges.slice(0, 7) as [k, n] (k)}<span>{k.replace('_', ' ')} {n.toLocaleString()}</span>{/each}
		</p>
		{#if health.warnings?.length}<p class="tr-nudge tr-warn">{health.warnings[0]}</p>{/if}
	{/if}
	{#if error}<p class="tr-line tr-err">{error}</p>{/if}
</header>

<aside class="tr-lane" class:idle={!pickedId} aria-live="polite">
	{#if pickedId && picked}
		<MemoryFocus
			id={pickedId}
			onpick={focusPick}
			onlinks={(l) => (focusLinks = l)}
			onverify={(v) => (verify = v)}
			onclose={() => (pickedId = null)}
		>
			{#if reviewById.get(pickedId)}
				{@const r = reviewById.get(pickedId)!}
				<p class="tr-note">The dream just reviewed it: rating {r.rating}, stability {Math.round(r.stability ?? 0)} days.</p>
			{/if}
		</MemoryFocus>
	{:else if busy}
		<p class="tr-proof-line">Running…</p>
	{:else if lens === 'durable'}
		<p class="tr-proof-kicker" use:alive={{ text: 'What a CLAUDE.md projection would carry' }}></p>
		<p class="tr-note">{river.length} durable memories. Each line in the file names its memory id; only the fenced region is ever replaced.</p>
		{#if durableRegion}<pre class="tr-note" style="white-space: pre-wrap; max-height: 16rem; overflow: auto">{durableRegion.slice(0, 2400)}</pre>{/if}
		<input class="tr-field" bind:value={projectPath} placeholder="absolute path of a CLAUDE.md or MEMORY.md" spellcheck="false" />
		<div class="tr-actions">
			<button type="button" class="tr-act" disabled={!projectPath.trim()} onclick={() => void writeProjection()}>
				{projectConfirm ? 'Replace the fenced region in that file?' : 'Write the projection'}
			</button>
		</div>
		{#if projectNote}<p class="tr-note">{projectNote}</p>{/if}
	{:else if lens === 'dream' && dream}
		<p class="tr-proof-kicker" use:alive={{ text: 'The dream, just now' }}></p>
		<p class="tr-proof-line">
			{dream.memoriesReplayed ?? 0} memories replayed over {dream.edgesConsidered?.toLocaleString() ?? '?'} recorded edges,
			{dream.strengthened ?? 0} strengthened, in {((dream.durationMs ?? 0) / 1000).toFixed(1)} s.
		</p>
		{#if dream.discovery}<p class="tr-note">{dream.discovery}</p>{/if}
		{#each dream.reviews.slice(0, 10) as r (r.id)}
			<button type="button" class="tr-item"
				data-trace-id={r.id} onclick={() => (pickedId = r.id)}>
				<span class="tr-edge causal">rating {r.rating} · {Math.round(r.stability ?? 0)} d</span>
				<span>{headline(byId.get(r.id)?.content ?? r.id, 100)}</span>
			</button>
		{/each}
	{:else if lens === 'brief' && brief}
		<p class="tr-proof-kicker" use:alive={{ text: 'What an agent is handed when it opens' }}></p>
		<pre class="tr-note" style="white-space: pre-wrap">{brief}</pre>
	{:else if lens === 'collect' && collect}
		<p class="tr-proof-kicker" use:alive={{ text: 'Garbage collection, dry run' }}></p>
		<p class="tr-proof-line">
			{collect.candidateCount ?? 0} memories would be collected{collect.processed ? ` of ${collect.processed} examined` : ''}.
			Deletion is refused on Strata in 4.0; nothing was removed.
		</p>
	{:else if lens === 'verifier'}
		<p class="tr-proof-kicker" use:alive={{ text: 'The verifier, and who answers it' }}></p>
		{#if !verifier}
			<p class="tr-note">Reading the Sanhedrin hook's settings…</p>
		{:else}
			<p class="tr-note">
				OrcaRouter is the named provider behind this machine's Stop hook — one OpenAI-compatible
				endpoint, {verifier.model ? `answering with ${verifier.model}` : 'with no model chosen yet'}. <a
					class="tr-id" href={verifier.home} target="_blank" rel="noreferrer">orcarouter.ai</a
				>
			</p>
			<p class="tr-proof-line" data-orcarouter-key-state={verifier.keyPresent ? 'present' : 'absent'}>
				{#if verifier.keyPresent}
					A key is present ({verifier.keySource === 'environment' ? 'from the environment' : 'in the env file'});
					the hook reads it and the dashboard never does.
				{:else}
					No key yet. Paste one from orcarouter.ai: it goes straight into the hook's 0600 env file and is
					never shown again.
				{/if}
			</p>
			<input
				class="tr-field"
				type="password"
				name="orcarouter-key"
				aria-label="OrcaRouter API key"
				autocomplete="off"
				spellcheck="false"
				placeholder="sk-orca-…"
				bind:value={keyDraft}
			/>
			<div class="tr-actions">
				<button type="button" class="tr-act alt" disabled={!keyDraft.trim() || savingKey} onclick={() => void saveKey()}>
					{savingKey ? 'Storing…' : 'Store key'}
				</button>
				<button
					type="button"
					class="tr-act quiet"
					disabled={!verifier.keyPresent || loadingCatalog}
					onclick={() => void refreshCatalog()}
				>
					{loadingCatalog ? 'Reading catalog…' : 'Refresh catalog'}
				</button>
			</div>
			{#if keyNote}<p class="tr-note">{keyNote}</p>{/if}
			{#if catalogError}
				<p class="tr-line tr-err">{catalogError}</p>
			{:else if loadingCatalog && !catalog}
				<p class="tr-proof-line">Reading the live catalog from {verifier.modelsUrl}…</p>
			{:else if catalog && catalog.models.length === 0}
				<p class="tr-line tr-warn">
					The catalog answered with {catalog.catalogTotal ?? 0} models and none of them can answer a chat
					completion. Nothing was written.
				</p>
			{:else if catalog}
				<div class="orc-dd">
					<Dropdown
						label="verifier model"
						placeholder="choose from the live catalog"
						icon="sparkle"
						options={modelOptions}
						bind:value={chosenModel}
						onChange={(id) => void saveModel(id)}
					/>
				</div>
				<p class="tr-proof-line">
					{catalog.total} of {catalog.catalogTotal ?? catalog.total} catalog models can answer this hook, read at{' '}
					{when(catalog.fetchedAt)}.{#if savingModel} Saving…{/if}
				</p>
			{:else if !verifier.keyPresent}
				<p class="tr-note">The catalog stays empty until a key is stored above.</p>
			{/if}
			{#if catalogNote}<p class="tr-note">{catalogNote}</p>{/if}
			<p class="tr-note">
				Only models the gateway itself lists are offered: nothing is typed by hand, and a choice is written
				to {verifier.envPath} only after the catalog accepts it.
			</p>
		{/if}
	{:else if lens === 'tools'}
		<p class="tr-proof-kicker" use:alive={{ text: 'Every tool, and where it is live here' }}></p>
		{#each tools as t (t.name)}
			<a class="tr-item" href={osHref(TOOL_HOME[t.name] ?? '/system')}>
				<span class="tr-edge causal">{t.name} · {TOOL_HOME[t.name]?.slice(1) ?? 'system'}</span>
				<span>{headline(t.description, 120)}</span>
			</a>
		{/each}
	{/if}
</aside>

<style>
	/* The catalog dropdown fills the lane so its menu and trigger share the
	   same right edge; long model ids ellipsize instead of widening it. */
	.orc-dd {
		width: 100%;
		margin-top: 0.7rem;
	}
	.orc-dd :global(.dd) {
		width: 100%;
	}
	.orc-dd :global(.dd-trigger) {
		width: 100%;
	}
	/* The menu is exactly as wide as its trigger, so the panel and the list
	   share one right edge; a long model id ellipsizes inside it. The panel
	   paints an opaque surface of its own: the model list is the one place a
	   reader picks a provider, and the lane behind it must not bleed through. */
	.orc-dd :global(.dd-menu) {
		width: 100%;
		max-width: 100%;
		box-sizing: border-box;
		background: rgb(10, 10, 26);
	}
	.orc-dd :global(.dd-option),
	.orc-dd :global(.dd-opt-label) {
		min-width: 0;
	}
</style>
