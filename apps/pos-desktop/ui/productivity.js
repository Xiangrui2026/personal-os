(() => {
  const defaultZone = 'Asia/Shanghai';
  const settings = { root: sessionStorage.getItem('pos-vault-root') || '', zone: defaultZone, date: null };
  const labels = {
    goal: '目标', milestone: '里程碑', project: '项目', task: '任务',
    daily_plan: '日计划', event: '日程', time_block: '时间块', time_entry: '实际用时',
  };
  const statuses = { todo: '待办', doing: '进行中', done: '完成', cancelled: '取消' };

  function e(tag, attrs = {}, ...children) {
    const node = document.createElement(tag);
    for (const [name, value] of Object.entries(attrs)) {
      if (value === false || value === null || value === undefined) continue;
      if (name === 'class') node.className = value;
      else if (name === 'value') node.value = value;
      else node.setAttribute(name, value === true ? '' : value);
    }
    for (const child of children.flat(Infinity)) {
      if (child !== null && child !== undefined && child !== false) node.append(child);
    }
    return node;
  }

  function invoke(name, args) {
    if (!window.__TAURI__?.core?.invoke) return Promise.reject(new Error('请在 Personal OS 桌面客户端打开。'));
    return window.__TAURI__.core.invoke(name, args);
  }

  function action(text, callback, kind = '') {
    const button = e('button', { type: 'button', class: 'pos-button ' + kind }, text);
    button.addEventListener('click', callback);
    return button;
  }

  function section(title, ...content) {
    return e('section', { class: 'pos-panel' }, e('h2', {}, title), content);
  }

  function message(text) {
    return e('p', { class: 'pos-muted' }, text);
  }

  function field(label, control) {
    return e('label', { class: 'pos-field' }, e('span', {}, label), control);
  }

  function input(name, label, value = '', attrs = {}) {
    return field(label, e('input', { name, value, ...attrs }));
  }

  function list(items, empty) {
    return items.length ? e('ul', { class: 'pos-list' }, items.map(item => e('li', {}, item))) : message(empty);
  }

  function row(title, detail, buttons = []) {
    return e('div', { class: 'pos-row' },
      e('div', {}, e('strong', {}, title), detail ? e('p', {}, detail) : null),
      buttons.length ? e('div', { class: 'pos-actions' }, buttons) : null);
  }

  function nowDate(zone) {
    const parts = new Intl.DateTimeFormat('en-CA', {
      timeZone: zone, year: 'numeric', month: '2-digit', day: '2-digit',
    }).formatToParts(new Date());
    return ['year', 'month', 'day'].map(type => parts.find(part => part.type === type).value).join('-');
  }

  function timeText(timestamp, zone, localInput = false) {
    const parts = new Intl.DateTimeFormat('en-GB', {
      timeZone: zone, year: 'numeric', month: '2-digit', day: '2-digit',
      hour: '2-digit', minute: '2-digit', hourCycle: 'h23',
    }).formatToParts(new Date(timestamp));
    const part = type => parts.find(part => part.type === type)?.value;
    const time = part('hour') + ':' + part('minute');
    return localInput ? [part('year'), part('month'), part('day')].join('-') + 'T' + time : time;
  }

  function timerText(seconds) {
    const time = Math.max(0, Math.floor(seconds));
    return [Math.floor(time / 3600), Math.floor(time % 3600 / 60), time % 60]
      .map(value => String(value).padStart(2, '0')).join(':');
  }

  function mount(host, page) {
    const view = { host, page, version: 0, entries: [], board: null };
    const root = e('input', { value: settings.root, placeholder: '本地 Vault 文件夹路径', required: true });
    const connect = e('form', { class: 'pos-connect' },
      field('Vault 文件夹', root), e('button', { class: 'pos-button pos-primary', type: 'submit' }, '读取'),
    );
    const notice = e('p', { class: 'pos-error', role: 'status' });
    const content = e('div', { class: 'pos-content' });
    view.notice = notice;
    view.content = content;
    if (page === 'day') {
      const connection = e('details', { class: 'day-vault', open: !settings.root },
        e('summary', {}, 'Vault 连接'), connect);
      host.replaceChildren(notice, content, connection);
      view.connection = connection;
    } else host.replaceChildren(connect, notice, content);
    connect.addEventListener('submit', event => {
      event.preventDefault();
      settings.root = root.value.trim();
      sessionStorage.setItem('pos-vault-root', settings.root);
      load(view);
    });
    if (settings.root) load(view);
    else invoke('startup_vault', {}).then(path => {
      if (!path || !host.isConnected || settings.root) return;
      settings.root = path;
      root.value = path;
      load(view);
    }).catch(error => report(view, error));
  }

  function report(view, error) {
    view.notice.textContent = String(error?.message || error);
  }

  async function load(view) {
    const version = ++view.version;
    view.notice.textContent = '';
    view.content.replaceChildren(message('正在读取 Vault…'));
    const root = settings.root;
    view.root = root;
    try {
      const data = view.page === 'day'
        ? invoke('day', { root, date: settings.date, timezone: settings.zone })
        : view.page === 'roadmap' ? invoke('roadmap', { root }) : Promise.resolve(null);
      const [entries, result] = await Promise.all([invoke('entities', { root }), data]);
      if (!view.host.isConnected || view.version !== version) return;
      view.entries = entries;
      view.content.replaceChildren();
      if (view.page === 'day') {
        view.connection.open = false;
        view.board = result;
        settings.date = result.date;
        renderDay(view, result);
      } else if (view.page === 'roadmap') {
        renderRoadmap(view, result);
      } else {
        renderProjects(view);
      }
    } catch (error) {
      if (view.version !== version) return;
      view.content.replaceChildren();
      report(view, error);
    }
  }

  function renderDay(view, board) {
    window.PosDayBoard.render(view, board, {
      e, action, message, list, row, section, edit, load, closeDay, focus, preview,
      nowDate, timeText, statuses, settings,
    });
  }

  function focus(view, board) {
    if (!board.focus) return message('从任务看板选择一项任务开始专注。');
    const active = board.focus.status === 'running';
    const clock = e('strong', { class: 'pos-timer' }, timerText(board.focus_elapsed_seconds));
    if (active) {
      const since = Date.now();
      const timer = setInterval(() => {
        if (!clock.isConnected) { clearInterval(timer); return; }
        clock.textContent = timerText(board.focus_elapsed_seconds + (Date.now() - since) / 1000);
      }, 1000);
    }
    return e('div', { class: 'pos-focus' }, e('strong', {}, board.focus.title), clock,
      e('div', { class: 'pos-actions' },
        action(active ? '暂停' : '继续', () => preview(view, { action: active ? 'focus_pause' : 'focus_resume' })),
        action('结束计时', () => preview(view, { action: 'focus_stop' })),
      ));
  }

  function renderRoadmap(view, roadmap) {
    view.content.append(e('div', { class: 'pos-toolbar' },
      action('新建目标', () => edit(view, 'goal'), 'pos-primary'),
      action('新建里程碑', () => edit(view, 'milestone')),
      action('刷新', () => load(view))));
    if (!roadmap.length) view.content.append(section('目标路线图', message('暂无目标')));
    for (const group of roadmap) {
      const stages = group.milestones.filter(stage => stage.status !== 'cancelled');
      view.content.append(section(group.goal.title,
        row(statuses[group.goal.status] || '', null, [action('编辑目标', () => edit(view, 'goal', group.goal))]),
        completion(group.goal.title, stages, '个里程碑'),
        list(group.milestones.map(stage => row(stage.sequence + ' · ' + stage.title,
          [stage.id === group.current_milestone ? '当前阶段' : statuses[stage.status], ...stage.exit_criteria].filter(Boolean).join(' · '),
          [action('编辑', () => edit(view, 'milestone', stage))])), '暂无里程碑'),
        group.projects.length ? message('关联项目 · ' + group.projects.map(project => project.title).join('、')) : null,
      ));
    }
  }

  function completion(title, items, unit) {
    if (!items.length) return null;
    const done = items.filter(item => item.status === 'done').length;
    return e('div', { class: 'day-roadmap-progress' },
      message(`${done} / ${items.length} ${unit}已完成`),
      window.PosDayBoard.meter({ e }, done / items.length, title + '进度'));
  }

  function renderProjects(view) {
    view.content.append(e('div', { class: 'pos-toolbar' },
      action('新建项目', () => edit(view, 'project'), 'pos-primary'),
      action('新建任务', () => edit(view, 'task')),
      action('刷新', () => load(view))));
    for (const kind of ['project', 'task']) {
      view.content.append(section(kind === 'project' ? '项目' : '任务',
        list(view.entries.filter(item => item.type === kind).map(item => e('div', {},
          row(item.title, statuses[item.status], [action('编辑', () => edit(view, kind, item))]),
          kind === 'project' ? completion(item.title, view.entries.filter(task => task.type === 'task'
            && task.project === item.id && task.status !== 'cancelled'), '项任务') : null)),
        kind === 'project' ? '暂无项目' : '暂无任务')));
    }
  }

  function formDialog(view, title, fields, label, submit) {
    const dialog = e('dialog', { class: 'pos-dialog' });
    const error = e('p', { class: 'pos-error', role: 'alert' });
    const save = e('button', { type: 'submit', class: 'pos-button pos-primary' }, label);
    const form = e('form', { class: 'pos-dialog-form' }, e('h2', {}, title), fields, error,
      e('div', { class: 'pos-actions pos-dialog-actions' }, action('取消', () => dialog.close()), save));
    form.addEventListener('submit', async event => {
      event.preventDefault();
      save.disabled = true;
      error.textContent = '';
      try {
        const next = await submit(new FormData(form));
        dialog.close();
        if (typeof next === 'function') next();
      } catch (failure) {
        error.textContent = String(failure?.message || failure);
      } finally {
        save.disabled = false;
      }
    });
    dialog.addEventListener('close', () => dialog.remove());
    dialog.append(form);
    view.host.append(dialog);
    dialog.showModal();
  }

  function choice(name, label, entries, value = '', required = false, multiple = false) {
    const select = e('select', { name, required, multiple, size: multiple ? 3 : null });
    if (!required && !multiple) select.append(e('option', { value: '' }, '—'));
    for (const [id, title] of entries) {
      const selected = multiple ? value.includes(id) : value === id;
      select.append(e('option', { value: id, selected }, title));
    }
    return field(label, select);
  }

  function dateField(view) {
    return view.board?.date || settings.date || nowDate(settings.zone);
  }

  function edit(view, kind, existing = null) {
    const day = dateField(view);
    const current = existing || {};
    const refs = type => view.entries.filter(item => item.type === type).map(item => [item.id, item.title]);
    const reference = (name, label, kind, required = false) => choice(name, label, refs(kind), current[name] || '', required);
    const references = (name, label, kind) => choice(name, label, refs(kind), current[name] || [], false, true);
    const numeric = (name, label, value, minimum = 0) => input(name, label, value ?? '', { type: 'number', min: minimum, max: 4294967295, step: 1 });
    const status = () => choice('status', '状态', Object.entries(statuses), current.status || 'todo', true);
    const interval = current.interval || current.intervals?.[0];
    const currentWindows = current.work_windows?.map(interval => {
      const start = timeText(interval.start, settings.zone, true);
      const end = timeText(interval.end, settings.zone, true);
      return start.slice(11) + '-' + (end.slice(0, 10) !== current.date && end.slice(11) === '00:00' ? '24:00' : end.slice(11));
    });
    const checkbox = (name, label, checked) => e('label', { class: 'pos-check' },
      e('input', { type: 'checkbox', name, checked }), e('span', {}, label));
    const controls = [input('title', '标题', current.title || (kind === 'daily_plan' ? '日计划 ' + day : ''), { required: true })];
    if (['goal', 'milestone', 'project', 'task'].includes(kind)) controls.push(status());
    if (kind === 'milestone') controls.push(reference('goal', '所属目标', 'goal', true),
      numeric('sequence', '阶段顺序', current.sequence ?? 1, 1),
      input('due_at', '截止时间', current.due_at ? timeText(current.due_at, settings.zone, true) : '', { type: 'datetime-local' }),
      field('验收条件（每行一项）', e('textarea', { name: 'exit_criteria', rows: 3 }, (current.exit_criteria || []).join('\n'))));
    if (kind === 'project') controls.push(references('goals', '关联目标', 'goal'), references('milestones', '关联里程碑', 'milestone'));
    if (kind === 'task') controls.push(reference('project', '所属项目', 'project'), references('goals', '关联目标', 'goal'),
      reference('milestone', '关联里程碑', 'milestone'), references('depends_on', '前置任务', 'task'),
      input('due_at', '截止时间', current.due_at ? timeText(current.due_at, settings.zone, true) : '', { type: 'datetime-local' }),
      numeric('estimate_minutes', '预计耗时（分钟）', current.estimate_minutes ?? current.initial_estimate_minutes, 1),
      input('category', '类别', current.category || ''),
      input('importance', '重要性（0–100）', current.importance ?? '', { type: 'number', min: 0, max: 100, step: 1 }),
      field('检查点（每行一项）', e('textarea', { name: 'checkpoints', rows: 3 },
        (current.checkpoints || []).map(item => item.title).join('\n'))),
      field('实际产出', e('textarea', { name: 'outcome', rows: 2 }, current.outcome || '')),
      checkbox('exclude_from_estimates', '范围变更或记录不完整，排除出历史估时', current.exclude_from_estimates));
    if (kind === 'daily_plan') controls.push(input('date', '日期', current.date || day, { type: 'date', readonly: Boolean(existing), required: true }),
      references('tasks', '安排任务', 'task'), references('priority_tasks', '重点任务', 'task'),
      references('focus_goals', '当日重点目标', 'goal'), numeric('budget_minutes', '预算（分钟）', current.budget_minutes),
      numeric('buffer_minutes', '缓冲（分钟）', current.buffer_minutes ?? 0),
      numeric('strategic_minutes', '战略时间目标（分钟）', current.strategic_minutes),
      input('importance_threshold', '四象限重要性阈值', current.importance_threshold ?? '', { type: 'number', min: 1, max: 99, step: 1, placeholder: '70' }),
      input('urgency_percent', '四象限紧迫度阈值（%）', current.urgency_percent ?? '', { type: 'number', min: 1, max: 1000, step: 1, placeholder: '70' }),
      checkbox('configure_windows', '明确配置当天可工作时段（日休可留空）', current.work_windows != null),
      field('可工作时段（每行一段）', e('textarea', { name: 'work_windows', rows: 3, placeholder: '09:00-12:00\n14:00-18:00' },
        currentWindows?.join('\n') || '')));
    if (kind === 'time_block' || kind === 'time_entry') controls.push(reference('task', '关联任务', 'task', true));
    if (['event', 'time_block', 'time_entry'].includes(kind)) {
      controls.push(input('start', '开始', interval ? timeText(interval.start, settings.zone, true) : day + 'T09:00', { type: 'datetime-local', required: true }),
        input('end', '结束', interval ? timeText(interval.end, settings.zone, true) : day + 'T10:00', { type: 'datetime-local', required: true }));
    }
    if (kind === 'time_entry' && (current.intervals?.length || 0) > 1) {
      report(view, '这条专注记录包含多个区间，可通过 CLI 编辑完整记录。');
      return;
    }
    if (kind === 'task' && existing) controls.push(input('estimate_reason', '估时调整原因（可选）'));
    formDialog(view, (existing ? '编辑' : '新建') + labels[kind], controls, '预览变更', async form => {
      const value = name => String(form.get(name) || '').trim();
      const all = name => form.getAll(name);
      const number = name => value(name) ? Number(value(name)) : null;
      const lines = name => value(name).split('\n').map(text => text.trim()).filter(Boolean);
      const instant = async name => value(name)
        ? invoke('wall_time', { time: value(name), timezone: settings.zone }) : null;
      const item = { type: kind, title: value('title') };
      if (form.has('status')) item.status = value('status');
      if (kind === 'milestone') Object.assign(item, { goal: value('goal'), sequence: number('sequence'),
        due_at: await instant('due_at'), exit_criteria: lines('exit_criteria') });
      if (kind === 'project') Object.assign(item, { goals: all('goals'), milestones: all('milestones') });
      if (kind === 'task') Object.assign(item, { project: value('project') || null, goals: all('goals'), milestone: value('milestone') || null,
        depends_on: all('depends_on'), due_at: await instant('due_at'),
        initial_estimate_minutes: current.initial_estimate_minutes ?? number('estimate_minutes'),
        estimate_minutes: number('estimate_minutes'), category: value('category') || null,
        importance: number('importance'), outcome: value('outcome') || null,
        exclude_from_estimates: form.has('exclude_from_estimates'),
        checkpoints: lines('checkpoints').map(title => ({
          title, done: current.checkpoints?.some(item => item.title === title && item.done) || false,
        })) });
      if (kind === 'daily_plan') Object.assign(item, { date: value('date'), tasks: all('tasks'),
        priority_tasks: all('priority_tasks'), focus_goals: all('focus_goals'),
        budget_minutes: number('budget_minutes'), buffer_minutes: number('buffer_minutes') || 0,
        strategic_minutes: number('strategic_minutes'), importance_threshold: number('importance_threshold'),
        urgency_percent: number('urgency_percent'), closing: current.closing || null,
        work_windows: currentWindows && form.has('configure_windows') && value('work_windows') === currentWindows.join('\n')
          ? current.work_windows : form.has('configure_windows') || lines('work_windows').length
          ? await Promise.all(lines('work_windows').map(async line => {
            const match = /^((?:[01]\d|2[0-3]):[0-5]\d)\s*-\s*((?:[01]\d|2[0-3]):[0-5]\d|24:00)$/.exec(line);
            if (!match || match[1] >= match[2]) throw new Error('可工作时段应按 09:00-12:00 填写，且结束需晚于开始。');
            const [start, end] = await Promise.all(match.slice(1).map(time => {
              const date = new Date(value('date') + 'T12:00:00Z');
              if (time === '24:00') date.setUTCDate(date.getUTCDate() + 1);
              return invoke('wall_time', { time: date.toISOString().slice(0, 10) + 'T' + (time === '24:00' ? '00:00' : time), timezone: settings.zone });
            }));
            return { start, end };
          })) : null });
      if (['event', 'time_block', 'time_entry'].includes(kind)) {
        const times = { start: await instant('start'), end: await instant('end') };
        if (kind === 'time_entry') Object.assign(item, { task: value('task'), intervals: [times], status: 'finished' });
        else Object.assign(item, { interval: times, ...(kind === 'time_block' ? { task: value('task') } : {}) });
      }
      const command = existing ? { action: 'edit', id: existing.id, item } : { action: 'create', item };
      const plan = await invoke('preview', { root: view.root, action: command, reason: value('estimate_reason') || null });
      return () => confirm(view, plan);
    });
  }

  function closeDay(view) {
    const board = view.board;
    const tasks = board.tasks.filter(row => row.planned && ['todo', 'doing'].includes(row.task.status));
    const controls = tasks.map(({ task }) => e('fieldset', { class: 'pos-fieldset' },
      e('legend', {}, task.title),
      choice(task.id, '处理方式', [['inbox', '退回待安排'], ['move', '改期'], ['drop', '取消当日安排']], 'inbox', true),
      input(task.id + '-date', '改期日期', '', { type: 'date', min: board.date }),
    ));
    formDialog(view, '结束一天', controls.length ? controls : [message('当天没有需要重新安排的任务。')], '预览结案', async form => {
      const decisions = tasks.map(({ task }) => {
        const action = String(form.get(task.id));
        const date = String(form.get(task.id + '-date') || '');
        if (action === 'move' && !date) throw new Error('请选择改期日期。');
        return { task: task.id, action, ...(action === 'move' ? { date } : {}) };
      });
      const plan = await invoke('preview', { root: view.root, action: { action: 'close_day', date: board.date, decisions }, reason: null });
      return () => confirm(view, plan);
    });
  }

  async function preview(view, command) {
    view.notice.textContent = '';
    try {
      const plan = await invoke('preview', { root: view.root, action: command, reason: null });
      if (view.host.isConnected) confirm(view, plan);
    } catch (error) { report(view, error); }
  }

  function confirm(view, plan) {
    if (!plan.vault_id) {
      report(view, '预览已生成。应用变更前需在获准的 Vault 初始化 Change 版本库。');
      return;
    }
    formDialog(view, '确认变更', [
      e('p', {}, plan.request.path),
      e('div', { class: 'pos-diff' },
        e('pre', {}, plan.patch.before || '（新建）'),
        e('pre', {}, plan.patch.after || '（空）')),
      e('label', { class: 'pos-check' }, e('input', { type: 'checkbox', name: 'exclusive', required: true }),
        e('span', {}, '我确认写入此文件，并已暂停其他会修改它的程序。')),
    ], '应用变更', async form => {
      await invoke('apply', { root: view.root, plan, exclusive: form.has('exclusive') });
      return () => load(view);
    });
  }

  window.PosProductivity = { mount };
})();
