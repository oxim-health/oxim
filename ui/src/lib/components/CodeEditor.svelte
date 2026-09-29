<script lang="ts">
  // A YAML editor (CodeMirror 6). The editor lives in a shadow root: there
  // CodeMirror adds its styles as constructable style sheets, which the
  // server's Content-Security-Policy allows (in the main document it would
  // insert style elements, which the policy blocks). The app's theme
  // variables inherit into the shadow root.
  import { onDestroy } from 'svelte';
  import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands';
  import { yaml } from '@codemirror/lang-yaml';
  import {
    bracketMatching,
    foldGutter,
    HighlightStyle,
    indentOnInput,
    syntaxHighlighting,
  } from '@codemirror/language';
  import { lintGutter, setDiagnostics, type Diagnostic } from '@codemirror/lint';
  import { highlightSelectionMatches, searchKeymap } from '@codemirror/search';
  import { Compartment, EditorState } from '@codemirror/state';
  import {
    drawSelection,
    EditorView,
    highlightActiveLine,
    highlightActiveLineGutter,
    keymap,
    lineNumbers,
  } from '@codemirror/view';
  import { tags } from '@lezer/highlight';
  import type { YamlProblem as EditorProblem } from '../channel-yaml';

  let {
    value = $bindable(''),
    label,
    readonly = false,
    problems = [],
    description,
  }: {
    value?: string;
    label: string;
    readonly?: boolean;
    problems?: EditorProblem[];
    description?: string;
  } = $props();

  const highlight = HighlightStyle.define([
    { tag: tags.propertyName, color: 'var(--syn-key)' },
    { tag: [tags.string, tags.special(tags.string)], color: 'var(--syn-string)' },
    { tag: [tags.number, tags.bool, tags.null], color: 'var(--syn-number)' },
    { tag: tags.comment, color: 'var(--syn-comment)', fontStyle: 'italic' },
    { tag: [tags.keyword, tags.meta, tags.definition(tags.propertyName)], color: 'var(--syn-keyword)' },
  ]);

  const theme = EditorView.theme({
    '&': {
      backgroundColor: 'var(--surface)',
      color: 'var(--text)',
      border: '1px solid var(--border-strong)',
      borderRadius: 'var(--radius)',
      fontSize: '0.88rem',
    },
    '&.cm-focused': { outline: '3px solid var(--focus)', outlineOffset: '2px' },
    '.cm-content': { fontFamily: 'var(--mono)', caretColor: 'var(--text)', minHeight: '20rem' },
    '.cm-gutters': {
      backgroundColor: 'var(--surface-2)',
      color: 'var(--muted)',
      borderRight: '1px solid var(--border)',
    },
    '.cm-activeLine': { backgroundColor: 'color-mix(in srgb, var(--accent) 7%, transparent)' },
    '.cm-activeLineGutter': { backgroundColor: 'color-mix(in srgb, var(--accent) 12%, transparent)' },
    '.cm-selectionBackground, &.cm-focused .cm-selectionBackground': {
      backgroundColor: 'color-mix(in srgb, var(--focus) 30%, transparent) !important',
    },
    '.cm-cursor': { borderLeftColor: 'var(--text)' },
    '.cm-scroller': { maxHeight: '60vh', overflow: 'auto' },
    '.cm-tooltip': { backgroundColor: 'var(--surface)', color: 'var(--text)', border: '1px solid var(--border)' },
    '.cm-diagnostic-error': { borderLeftColor: 'var(--bad)' },
  });

  const editable = new Compartment();

  function editing(locked: boolean) {
    return [EditorState.readOnly.of(locked), EditorView.editable.of(!locked)];
  }

  let host: HTMLDivElement | undefined = $state();
  let view: EditorView | undefined;

  $effect(() => {
    if (!host || view) return;
    const shadow = host.shadowRoot ?? host.attachShadow({ mode: 'open' });
    view = new EditorView({
      parent: shadow,
      root: shadow,
      state: EditorState.create({
        doc: value,
        extensions: [
          lineNumbers(),
          highlightActiveLineGutter(),
          foldGutter(),
          lintGutter(),
          history(),
          drawSelection(),
          indentOnInput(),
          bracketMatching(),
          highlightActiveLine(),
          highlightSelectionMatches(),
          syntaxHighlighting(highlight),
          yaml(),
          theme,
          keymap.of([...defaultKeymap, ...historyKeymap, ...searchKeymap, indentWithTab]),
          editable.of(editing(readonly)),
          // aria-describedby cannot point out of a shadow root, so the
          // label carries the essentials and problems are announced by the
          // page's live region. An explicit tabindex (the content is
          // focusable anyway) lets accessibility checkers see that the
          // scroll area is keyboard-reachable.
          EditorView.contentAttributes.of({
            'aria-label': description ? `${label}. ${description}` : label,
            'aria-multiline': 'true',
            tabindex: '0',
          }),
          // When typed text replaces a selection, the browser would carry the
          // style of the removed text over in inline-styled spans (blocked by
          // the Content-Security-Policy). Apply such replacements directly;
          // ordinary typing and input-method composition stay native.
          EditorView.domEventHandlers({
            beforeinput(event, target) {
              if (event.inputType !== 'insertText' || event.data === null || target.composing) return false;
              if (target.state.selection.ranges.every((range) => range.empty)) return false;
              event.preventDefault();
              target.dispatch(
                target.state.update(target.state.replaceSelection(event.data), {
                  userEvent: 'input.type',
                  scrollIntoView: true,
                }),
              );
              return true;
            },
          }),
          EditorView.updateListener.of((update) => {
            if (update.docChanged) value = update.state.doc.toString();
          }),
        ],
      }),
    });
  });

  // Outside changes (a loaded file, a reset) replace the document.
  $effect(() => {
    const text = value;
    if (view && text !== view.state.doc.toString()) {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text } });
    }
  });

  $effect(() => {
    view?.dispatch({
      effects: editable.reconfigure(editing(readonly)),
    });
  });

  $effect(() => {
    if (!view) return;
    const doc = view.state.doc;
    const diagnostics: Diagnostic[] = problems.map((problem) => {
      const lineNumber = Math.min(Math.max(problem.line ?? 1, 1), doc.lines);
      const line = doc.line(lineNumber);
      const from = Math.min(line.from + Math.max((problem.column ?? 1) - 1, 0), line.to);
      return { from, to: problem.line ? line.to : from, severity: 'error', message: problem.message };
    });
    view.dispatch(setDiagnostics(view.state, diagnostics));
  });

  onDestroy(() => view?.destroy());
</script>

<div class="editor" bind:this={host}></div>
<p class="hint muted small-text">Tab indents. Press Escape, then Tab, to move focus out of the editor.</p>

<style>
  .hint {
    margin: 0.25rem 0 0;
  }
</style>
