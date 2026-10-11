//! The JavaScript the driver itself runs in a page.
//!
//! One function, called with `Runtime.callFunctionOn` in an isolated world:
//! it shares the page's DOM and none of its script state, so a page that
//! replaced `JSON` or a prototype cannot change what these helpers do. The
//! first argument names the operation and `this` is the element it applies
//! to, or the window for the ones about the whole document.
//!
//! Whatever a helper returns is page data and is bounded again in Rust.

pub(crate) const LIBRARY: &str = r#"function (operation, first, second) {
  const shown = element => {
    const rect = element.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0 && getComputedStyle(element).visibility === 'visible'
      && (!element.checkVisibility || element.checkVisibility());
  };
  // The parent in the tree the user sees: through slots and out of shadow roots.
  const parent = element => element.assignedSlot || element.parentElement
    || (element.getRootNode() instanceof ShadowRoot ? element.getRootNode().host : null);
  // The control an element stands for: the button or link around an inner
  // node and, when asked, the field a label belongs to.
  const control = (element, followLabel) => {
    if (!element.matches('input, textarea, select') && !element.isContentEditable)
      element = element.closest('button, [role=button], a, [role=link]') || element;
    if (followLabel && !element.isContentEditable
      && !element.matches('a, input, textarea, button, select, [role=link], [role=button], [role=checkbox], [role=radio]')) {
      const label = element.closest('label');
      if (label && label.control) element = label.control;
    }
    return element;
  };
  const describe = element => {
    let text = '<' + element.nodeName.toLowerCase();
    if (element.id) text += ' id="' + element.id + '"';
    const names = element.getAttribute('class');
    if (names) text += ' class="' + names + '"';
    return (text + '>').slice(0, 160);
  };
  const fire = (element, type) => element.dispatchEvent(new Event(type, { bubbles: true, composed: type === 'input' }));
  const operations = {
    connected: element => element.isConnected,
    title: () => document.title,
    rect: element => {
      const rect = element.getBoundingClientRect();
      return { x: rect.x, y: rect.y, width: rect.width, height: rect.height, shown: shown(element) };
    },
    // Whether the element can take an action now, and where its click lands.
    async state(element, wants) {
      if (!element.isConnected) return { state: 'detached' };
      if (!shown(element)) return { state: 'hidden' };
      const target = control(element, wants.editable);
      if (wants.enabled && (target.matches(':disabled') || target.closest('[aria-disabled="true"]')))
        return { state: 'disabled' };
      if (wants.editable && (target.readOnly || target.getAttribute('aria-readonly') === 'true'))
        return { state: 'readonly' };
      const box = () => {
        const rect = [...element.getClientRects()].find(rect => rect.width > 0 && rect.height > 0)
          || element.getBoundingClientRect();
        return [rect.left, rect.top, rect.width, rect.height];
      };
      let rect = box();
      if (wants.stable) {
        // One animation frame later, or a short while when none is produced.
        await new Promise(done => {
          requestAnimationFrame(done);
          setTimeout(done, 100);
        });
        const later = box();
        if (later.some((value, index) => Math.abs(value - rect[index]) > 0.5)) return { state: 'moving' };
        rect = later;
      }
      const left = Math.max(rect[0], 0), top = Math.max(rect[1], 0);
      const right = Math.min(rect[0] + rect[2], innerWidth), bottom = Math.min(rect[1] + rect[3], innerHeight);
      if (right <= left || bottom <= top) return { state: 'offscreen' };
      const x = (left + right) / 2, y = (top + bottom) / 2;
      if (wants.hit) {
        let hit = document.elementFromPoint(x, y);
        while (hit && hit.shadowRoot) {
          const inner = hit.shadowRoot.elementFromPoint(x, y);
          if (!inner || inner === hit) break;
          hit = inner;
        }
        let inside = false;
        for (let node = hit; node && !inside; node = parent(node)) inside = node === element;
        if (!inside) return { state: 'covered', by: hit ? describe(hit) : '' };
      }
      return { state: 'ready', x, y };
    },
    // Prepares a field for replacement text. `done` means the value is
    // already set; otherwise its content is selected for the insertion.
    fill(element, text) {
      element = control(element, true);
      if (element.nodeName === 'INPUT') {
        const type = (element.getAttribute('type') || '').toLowerCase();
        if (['color', 'date', 'time', 'datetime-local', 'month', 'range', 'week'].includes(type)) {
          const value = text.trim();
          element.focus();
          element.value = value;
          if (element.value !== value) return { error: 'Malformed value' };
          fire(element, 'input');
          fire(element, 'change');
          return { done: true };
        }
        if (!['', 'email', 'number', 'password', 'search', 'tel', 'text', 'url'].includes(type))
          return { error: 'Input of type "' + type + '" cannot be filled' };
        if (type === 'number' && isNaN(Number(text.trim()))) return { error: 'Cannot type text into input[type=number]' };
        element.select();
        element.focus();
        return { done: false };
      }
      if (element.nodeName === 'TEXTAREA') {
        element.selectionStart = 0;
        element.selectionEnd = element.value.length;
        element.focus();
        return { done: false };
      }
      if (element.isContentEditable) {
        const range = document.createRange();
        range.selectNodeContents(element);
        const selection = getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        element.focus();
        return { done: false };
      }
      return { error: 'Element is not an <input>, <textarea> or [contenteditable] element' };
    },
    // Each value matches an option's value or its label.
    select(element, values) {
      element = control(element, true);
      if (element.nodeName !== 'SELECT') return { error: 'Element is not a <select> element' };
      const picked = [];
      let remaining = values.slice();
      for (const option of element.options) {
        const matches = value => option.value === value || option.label === value;
        if (!remaining.some(matches)) continue;
        if (option.matches(':disabled')) return { missing: true };
        picked.push(option);
        if (!element.multiple) { remaining = []; break; }
        remaining = remaining.filter(value => !matches(value));
      }
      if (remaining.length) return { missing: true };
      element.value = undefined;
      for (const option of picked) option.selected = true;
      fire(element, 'input');
      fire(element, 'change');
      return { selected: picked.length };
    },
    checked(element) {
      element = control(element, true);
      if (element.nodeName === 'INPUT' && ['checkbox', 'radio'].includes(element.type)) return { checked: element.checked };
      if (['checkbox', 'radio', 'switch', 'menuitemcheckbox', 'menuitemradio'].includes(element.getAttribute('role')))
        return { checked: element.getAttribute('aria-checked') === 'true' };
      return { error: 'Not a checkbox or radio button' };
    },
    // What the user clicks when the real input is hidden: its label, or the
    // closest thing around it that is on screen.
    stand_in(element) {
      element = control(element, true);
      for (const label of element.labels || []) if (shown(label)) return label;
      for (let node = parent(element); node; node = parent(node)) if (shown(node)) return node;
      return null;
    },
    // The one element a selector names, or how many it matches. Open shadow
    // roots are searched too.
    query(_, selector) {
      const found = [];
      const search = scope => {
        for (const match of scope.querySelectorAll(selector)) found.push(match);
        for (const host of scope.querySelectorAll('*')) if (host.shadowRoot) search(host.shadowRoot);
      };
      search(document);
      return found.length === 1 ? found[0] : found.length;
    },
    // Whether some visible element shows the text. Matching ignores case and
    // runs of whitespace; the smallest element holding the text decides.
    text(_, needle) {
      const normal = value => value.replace(/[​­]/g, '').trim().replace(/\s+/g, ' ').toLowerCase();
      needle = normal(needle);
      const skipped = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE']);
      const holds = element => !skipped.has(element.nodeName) && normal(element.textContent || '').includes(needle);
      const visit = element => {
        let deeper = false;
        for (const child of element.children) {
          if (!holds(child)) continue;
          deeper = true;
          if (visit(child)) return true;
        }
        return !deeper && shown(element);
      };
      return !!document.body && holds(document.body) && visit(document.body);
    },
    // The selected text. For a cut, also what must still hold when it is confirmed.
    selection(_, options) {
      const element = document.activeElement;
      const input = element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement;
      if (input && element instanceof HTMLInputElement && element.type === 'password')
        return { error: 'Los campos de contraseña no se copian ni se cortan.' };
      const editable = input && typeof element.selectionStart === 'number' && typeof element.selectionEnd === 'number';
      if (options.cut && (!editable || element.readOnly || element.disabled))
        return { error: 'Cortar sólo está disponible en campos de texto editables.' };
      const start = editable ? element.selectionStart : 0;
      const end = editable ? element.selectionEnd : 0;
      const text = editable ? element.value.slice(start, end) : String(getSelection() || '');
      if (!text) return { error: 'Seleccioná el texto que querés copiar.' };
      if (text.length > options.maximum || new TextEncoder().encode(text).length > options.maximum || text.includes('\0'))
        return { error: 'La selección supera el límite de 16 KiB; copiá una parte más pequeña.' };
      if (options.cut && element.value.length > 256 * 1024)
        return { error: 'Este campo es demasiado extenso para cortarlo de forma segura.' };
      return { element: options.cut ? element : null, value: options.cut ? element.value : null, start, end, text };
    },
    selected: saved => ({ error: saved.error, text: saved.text }),
    // Removes the selection a cut was prepared for, only if nothing changed since.
    cut(saved) {
      const element = saved.element;
      const unchanged = () => element && element.isConnected && document.activeElement === element
        && !element.readOnly && !element.disabled && element.value === saved.value
        && element.selectionStart === saved.start && element.selectionEnd === saved.end;
      if (!unchanged()) return { error: 'El campo o su selección cambiaron; no se cortó texto.' };
      const before = new InputEvent('beforeinput', { bubbles: true, cancelable: true, inputType: 'deleteByCut' });
      if (!element.dispatchEvent(before) || !unchanged()) return { error: 'La página canceló el corte; no se cortó texto.' };
      element.setRangeText('', saved.start, saved.end, 'start');
      element.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'deleteByCut' }));
      return {};
    },
    // Notes whether the next pointer move starts an HTML drag in this frame.
    watch_drag(element) {
      globalThis.dragStarted = false;
      element.ownerDocument.defaultView.addEventListener(
        'dragstart', () => { globalThis.dragStarted = true; }, { capture: true, once: true });
      return true;
    },
    drag_started() {
      const started = globalThis.dragStarted === true;
      globalThis.dragStarted = false;
      return started;
    },
  };
  return operations[operation](this, first, second);
}"#;

/// Runs in the page's own world, on the value the caller's expression
/// produced: a function is called (with the element, if one was given) and
/// awaited, and the result is serialised in the page so that only bounded
/// text crosses to the driver.
pub(crate) const EVALUATE: &str = r#"async function (element, limit) {
  const value = await (typeof this === 'function' ? this(element) : this);
  let text;
  try { text = JSON.stringify(value); } catch { /* A BigInt or a cycle inside the value. */ }
  if (text === undefined) text = String(value);
  return { text: text.length > limit ? text.slice(0, limit) : text, cut: text.length > limit };
}"#;
