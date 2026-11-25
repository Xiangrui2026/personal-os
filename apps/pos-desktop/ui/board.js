(() => {
  const isOpen = task => task.status === 'todo' || task.status === 'doing';
  const rounded = value => value == null ? '—' : Math.round(value) + ' 分钟';
  const ratio = value => value == null ? '—' : value.toFixed(2) + '×';
  const palettes = ['#007e76', '#4660b3', '#a75927', '#864d93', '#48763c'];

  function render(view, board, ui) {
    const { e, action, message, edit, load, closeDay, settings } = ui;
    const today = ui.nowDate(board.timezone) === board.date;
    const closed = board.plan?.closing != null;
    const date = e('input', { type: 'date', value: board.date, 'aria-label': '看板日期' });
    date.onchange = () => { if (date.value) { settings.date = date.value; load(view); } };
    const changeDate = delta => {
      const date = new Date(board.date + 'T12:00:00Z');
      date.setUTCDate(date.getUTCDate() + delta);
      settings.date = date.toISOString().slice(0, 10);
      load(view);
    };
    const dateTitle = new Intl.DateTimeFormat('zh-CN', {
      month: 'long', day: 'numeric', weekday: 'long', timeZone: 'UTC',
    }).format(new Date(board.date + 'T12:00:00Z'));
    const hero = e('header', { class: 'day-hero' },
      e('div', {}, e('p', { class: 'day-date' }, `${dateTitle} · ${board.date.slice(0, 4)}`),
        e('h1', { id: 'page-title' }, '每日看板'),
        e('p', { class: 'day-state' }, closed ? '当日已结案' : today ? '今天' : '查看所选日期')),
      e('div', { class: 'day-hero-controls' },
        e('div', { class: 'day-date-controls' },
          arrow(ui, '‹', '前一天', () => changeDate(-1)), date,
          arrow(ui, '›', '后一天', () => changeDate(1)),
          action('今天', () => { settings.date = null; load(view); })),
        e('div', { class: 'day-date-controls' },
          action('编辑日计划', () => edit(view, 'daily_plan', board.plan)),
          action('新建任务', () => edit(view, 'task'), 'pos-primary'))));
    const { totals, planning } = board;
    const summary = e('div', { class: 'day-metrics' },
      metric(ui, '任务完成', `${totals.completed} / ${totals.tasks}`,
        totals.tasks ? Math.round(totals.completed / totals.tasks * 100) + '%' : '当天暂无任务',
        totals.tasks ? totals.completed / totals.tasks : null),
      metric(ui, '计划工作', rounded(totals.planned_work_seconds / 60),
        '固定日程 ' + rounded(totals.fixed_event_seconds / 60)),
      metric(ui, '实际投入', rounded(totals.actual_seconds / 60),
        totals.actual_seconds ? '区间已去重' : '当天暂无记录'),
      metric(ui, '剩余工作负荷', ratio(planning.workload_ratio),
        planning.remaining_work_minutes != null
          ? '已估 ' + rounded(totals.remaining_estimate_minutes) + ' · 可用 ' + rounded(planning.remaining_work_minutes)
          : '设置可工作时段后计算',
        planning.workload_ratio, planning.workload_ratio > 1));
    const views = [
      ['work', '执行看板', execution],
      ['time', '时间轴', calendar],
      ['plan', '规划分析', analysis],
    ];
    const content = e('div', { class: 'day-tab-content', id: 'day-view' });
    const tabs = e('nav', { class: 'day-tabs', 'aria-label': '每日看板视图' });
    const show = id => {
      view.dayTab = id;
      for (const button of tabs.children) button.setAttribute('aria-pressed', String(button.dataset.tab === id));
      const active = views.find(([key]) => key === id);
      content.replaceChildren(active[2](view, board, ui));
    };
    for (const [id, title] of views) {
      const button = action(title, () => show(id));
      button.dataset.tab = id;
      button.setAttribute('aria-controls', 'day-view');
      tabs.append(button);
    }
    const zone = e('input', { value: board.timezone, 'aria-label': 'IANA 时区', class: 'pos-zone' });
    zone.onchange = () => { if (zone.value.trim()) { settings.zone = zone.value.trim(); load(view); } };
    view.content.replaceChildren(hero, summary,
      e('div', { class: 'day-view-controls' }, tabs,
        e('details', { class: 'day-settings' }, e('summary', {}, '看板设置'),
          e('div', { class: 'pos-toolbar' }, zone, action('刷新', () => load(view)),
            closed ? message('当日已结案') : action('结束一天', () => closeDay(view))))), content);
    show(view.dayTab || 'work');
  }

  function arrow(ui, character, label, handler) {
    const button = ui.action(character, handler, 'day-arrow');
    button.setAttribute('aria-label', label);
    return button;
  }

  function meter(ui, value, label, warning = false) {
    const bar = ui.e('progress', {
      class: 'day-meter' + (warning ? ' day-warning' : ''), max: 1,
      'aria-label': label,
    });
    bar.value = Math.min(1, Math.max(0, value));
    return bar;
  }

  function metric(ui, label, value, caption, fraction = null, warning = false) {
    const { e } = ui;
    return e('article', { class: 'day-metric' }, e('span', {}, label), e('strong', {}, value),
      e('small', {}, caption), fraction == null ? null : meter(ui, fraction, label, warning));
  }

  function execution(view, board, ui) {
    const { e, action, preview, edit, list, message, row, section } = ui;
    const closed = board.plan?.closing != null;
    const priorities = board.tasks.filter(item => item.priority);
    const planning = e('aside', { class: 'day-stack' },
      section('今日重点', list(priorities.map(({ task }) => row(task.title, ui.statuses[task.status])), '暂无重点任务')),
      section('目标方向', list(board.focus_goals.map(group => row(group.goal.title,
        group.milestones.find(stage => stage.id === group.current_milestone)?.title)), '暂无重点目标')),
      section('待安排', e('details', { class: 'day-inbox', open: board.inbox.length <= 4 },
        e('summary', {}, `${board.inbox.length} 项任务`), list(board.inbox.map(task => row(task.title, task.category,
          [closed ? null : action('排入当天', () => preview(view, {
            action: 'schedule_task', date: board.date, task: task.id,
          })), action('编辑', () => edit(view, 'task', task))])), '待安排清单已清空'))));
    const lanes = Object.entries(ui.statuses).map(([status, label]) => {
      const entries = board.tasks.filter(item => item.task.status === status);
      return { status, label, entries };
    });
    const lane = ({ status, label, entries }) => e('section', { class: 'day-lane', 'data-status': status },
      e('h3', {}, e('span', { class: 'day-status-dot', 'aria-hidden': 'true' }), label,
        e('span', { class: 'day-lane-count' }, entries.length.toString())),
      entries.length ? entries.map(item => taskCard(view, board, ui, item)) : message('暂无任务'));
    const work = section('任务看板', e('div', { class: 'day-lanes' }, lanes.slice(0, 3).map(lane)),
      lanes[3].entries.length ? e('details', { class: 'day-cancelled' },
        e('summary', {}, `已取消 · ${lanes[3].entries.length} 项`), lane(lanes[3])) : null);
    const warnings = [];
    if (board.overlaps.length) warnings.push(`${board.overlaps.length} 组计划时间重叠`);
    const capacity = board.planning.remaining_work_minutes;
    if (capacity != null && board.totals.remaining_estimate_minutes > capacity) {
      warnings.push('剩余已估工作超出可用时间 ' + rounded(board.totals.remaining_estimate_minutes - capacity));
    }
    if (board.totals.unestimated_tasks) warnings.push(`${board.totals.unestimated_tasks} 项待执行任务缺少耗时估算`);
    const overdue = board.planning.priorities.filter(item => item.overdue).length;
    if (overdue) warnings.push(`${overdue} 项任务已超过截止时间或安排时点`);
    const outcomeItems = board.tasks.filter(({ task }) => task.status === 'done');
    const right = e('aside', { class: 'day-stack' },
      section('待关注', list(warnings, '暂无提醒')),
      section('任务产出', list(outcomeItems.map(({ task }) =>
        row(task.title, task.outcome || '尚未填写产出')), '当天看板暂无已完成任务')),
      section('时间分布', distribution(board, ui)));
    const center = e('div', { class: 'day-stack' }, section('专注计时', ui.focus(view, board)), work);
    return e('div', { class: 'day-execution' }, planning, center, right);
  }

  function taskCard(view, board, ui, entry) {
    const { e, action, preview, edit, message } = ui;
    const { task } = entry;
    const checkpoints = task.checkpoints || [];
    const done = checkpoints.filter(item => item.done).length;
    const options = [];
    if (isOpen(task) && !board.focus) options.push(action('专注', () => preview(view, { action: 'focus_start', task: task.id })));
    if (isOpen(task) && !entry.priority && board.plan?.closing == null) options.push(action('设为重点', () =>
      preview(view, { action: 'schedule_task', date: board.date, task: task.id, priority: true })));
    options.push(action('编辑', () => edit(view, 'task', task)));
    const editButton = action(task.title, () => edit(view, 'task', task), 'day-task-title');
    const estimate = task.estimate_minutes ?? task.initial_estimate_minutes;
    const metadata = [task.category, entry.priority ? '重点' : null, entry.blockers.length ? '有前置阻塞' : null].filter(Boolean);
    return e('article', { class: 'day-task' },
      metadata.length ? e('div', { class: 'day-tags' }, metadata.map(text => e('span', {}, text))) : null,
      e('h4', {}, editButton),
      entry.blockers.length ? e('p', { class: 'day-blockers' }, entry.blockers.map(id => view.entries.find(item => item.id === id)?.title || id).join('、')) : null,
      checkpoints.length ? e('details', { class: 'day-checkpoints' },
        e('summary', {}, `检查点 ${done}/${checkpoints.length}`),
        e('ul', {}, checkpoints.map(item => {
          const button = action((item.done ? '✓ ' : '○ ') + item.title, () =>
            preview(view, { action: 'set_checkpoint', id: task.id, title: item.title, done: !item.done }), 'day-checkpoint');
          button.setAttribute('aria-pressed', String(item.done));
          return e('li', {}, button);
        }))) : message('未拆分检查点'),
      checkpoints.length ? meter(ui, done / checkpoints.length, task.title + '的检查点进度') : null,
      estimate != null || entry.total_actual_seconds ? e('p', { class: 'day-task-time' },
        [estimate == null ? null : '估 ' + rounded(estimate), entry.total_actual_seconds ? '累计 ' + rounded(entry.total_actual_seconds / 60) : null].filter(Boolean).join(' · ')) : null,
      e('footer', {},
        isOpen(task) ? action(task.status === 'todo' ? '开始' : '完成', () => preview(view, {
          action: 'set_task_status', id: task.id, status: task.status === 'todo' ? 'doing' : 'done',
        }), 'day-status-action') : null,
        e('details', { class: 'day-task-options' }, e('summary', { 'aria-label': task.title + '的操作' }, '•••'),
          e('div', {}, options))));
  }

  function distribution(board, ui) {
    const { e, message } = ui;
    const shares = board.planning.distribution;
    if (!shares.length) return message('记录实际投入后显示');
    const total = shares.reduce((sum, item) => sum + item.seconds, 0);
    const color = (item, index) => item.overlap ? '#697779' : palettes[index % palettes.length];
    return e('div', { class: 'day-dist' },
      e('div', { class: 'day-dist-bar', 'aria-hidden': 'true' }, shares.map((item, index) => {
        const segment = e('span');
        segment.style.flexGrow = item.seconds / total;
        segment.style.backgroundColor = color(item, index);
        return segment;
      })),
      e('ul', {}, shares.map((item, index) => {
        const dot = e('i', { 'aria-hidden': 'true' });
        dot.style.backgroundColor = color(item, index);
        const label = item.overlap ? '跨任务重叠' : item.category || '未分类';
        return e('li', {}, dot, e('span', {}, label), e('strong', {}, rounded(item.seconds / 60)));
      })));
  }

  function calendar(view, board, ui) {
    const { e, action, message, section, edit, timeText } = ui;
    const start = +new Date(board.window.start);
    const end = +new Date(board.window.end);
    const extent = time => ((+new Date(time) - start) / (end - start) * 100) + '%';
    const clock = time => +new Date(time) === end ? '24:00' : timeText(time, board.timezone);
    const ticks = [];
    for (let tick = start; tick < end; tick += 4 * 3600000) {
      const label = e('span', {}, clock(tick));
      label.style.left = extent(tick);
      ticks.push(label);
    }
    const axis = e('div', { class: 'day-axis', 'aria-hidden': 'true' }, ticks);
    const markerTime = +new Date(board.as_of);
    const now = markerTime >= start && markerTime < end;
    function track(label, items, kind) {
      const wheel = e('div', { class: 'day-track-bands' });
      const lanes = [];
      for (const item of items) {
        const startAt = +new Date(item.start);
        let index = lanes.findIndex(lane => lane.end <= startAt);
        if (index < 0) {
          index = lanes.length;
          lanes.push({ node: e('div', { class: 'day-track-band' }) });
        }
        lanes[index].end = +new Date(item.end);
        const text = `${clock(item.start)}–${clock(item.end)} · ${item.title}`;
        const entity = kind === 'initial' ? null : view.entries.find(entity => entity.id === item.id);
        const editable = entity && !['paused', 'running'].includes(entity.status);
        const block = editable ? action(item.title, () => edit(view, entity.type, entity), 'day-time-block')
          : e('span', { class: 'day-time-block' }, item.title);
        block.dataset.kind = item.kind;
        block.title = text;
        if (editable) block.setAttribute('aria-label', text);
        block.style.left = extent(item.start);
        block.style.width = `calc(${extent(item.end)} - ${extent(item.start)})`;
        lanes[index].node.append(block);
      }
      wheel.append(...lanes.map(lane => lane.node));
      if (!items.length) wheel.append(message(kind === 'initial' && !board.initial ? '无可用的创建历史' : '暂无时间记录'));
      if (now) {
        const line = e('i', { class: 'day-now', 'aria-hidden': 'true', title: '当前 ' + clock(markerTime) });
        line.style.left = extent(markerTime);
        wheel.append(line);
      }
      const details = items.length ? e('details', { class: 'day-time-details' },
        e('summary', {}, `查看${label}明细 · ${items.length} 条`),
        e('ul', {}, items.map(item => e('li', {}, e('time', {}, `${clock(item.start)}–${clock(item.end)}`), item.title)))) : null;
      return e('div', { class: 'day-track' }, e('h3', {}, label), wheel, details);
    }
    const initialTitles = board.initial?.task_titles;
    const currentTitles = board.tasks.filter(row => row.planned).map(row => row.task.title);
    const chart = e('div', { class: 'day-timeline' }, axis,
      track('初始安排', board.initial?.scheduled || [], 'initial'),
      track('当前计划', board.scheduled, 'current'),
      track('实际记录', board.actual, 'actual'));
    return e('div', { class: 'day-stack' },
      section('计划和实际', e('div', { class: 'day-time-tools pos-actions' },
        action('新建日程', () => edit(view, 'event')),
        action('安排时间块', () => edit(view, 'time_block')),
        action('补录用时', () => edit(view, 'time_entry'), 'pos-primary')),
      e('div', { class: 'day-legend' }, e('span', { 'data-kind': 'event' }, '固定日程'),
        e('span', { 'data-kind': 'time_block' }, '任务时间块'),
        e('span', { 'data-kind': 'time_entry' }, '实际记录')), chart),
      e('div', { class: 'day-analysis-grid' },
        section('计划任务',
          message('首次保存 · ' + (initialTitles == null ? '无可用创建历史' : initialTitles.join('、') || '无')),
          message('当前安排 · ' + (currentTitles.join('、') || '无')),
          board.journal_path ? message('当日日记 · ' + board.journal_path) : null),
        section('时间分布', distribution(board, ui))));
  }

  function analysis(view, board, ui) {
    const { e, action, section, message, timeText } = ui;
    const model = board.planning;
    const importanceCutoff = model.importance_threshold;
    const urgencyCutoff = model.urgency_threshold;
    const names = new Map(board.tasks.map(row => [row.task.id, row.task.title]));
    const quadrants = ['重要且紧迫', '重要且从容', '紧迫', '暂缓'];
    const sources = { task: '任务截止', milestone: '里程碑', dependency: '后续任务', day: '日计划', block: '时间块' };
    const details = e('div', { class: 'day-point-details', role: 'status' });
    const pick = point => {
      details.replaceChildren(...[e('strong', {}, names.get(point.task)),
        message(`重要性 ${point.importance ?? '—'} · 紧迫度 ${point.overdue ? '已到时点' : point.exhausted ? '可用时间为零' : ratio(point.pressure)}`),
        message(`可用 ${rounded(point.available_minutes)} · 余量 ${rounded(point.slack_minutes)}`),
        point.p80_slack_minutes == null ? null :
          message('历史 P80 下的余量 ' + rounded(point.p80_slack_minutes)),
        message(point.deadline ? `${sources[point.deadline_source]} · ${timeText(point.deadline, board.timezone, true).replace('T', ' ')}` : '缺少截止或安排时点')].filter(Boolean));
    };
    const plot = e('div', { class: 'day-matrix', role: 'group', 'aria-label': '按紧迫度和重要性分布的任务' },
      ...[2, 1, 4, 3].map(number => e('div', { class: 'day-quadrant', 'data-n': number },
        e('strong', {}, quadrants[number - 1]),
        e('span', {}, model.priorities.filter(item => item.quadrant === number).length + ' 项'))));
    // Keep near-deadline values readable while retaining a continuous urgency scale.
    const urgentPosition = value => value == null ? 96 : value <= urgencyCutoff ? value / urgencyCutoff * 50 :
      50 + (1 - urgencyCutoff / value) * 47;
    for (const point of model.priorities.filter(item => item.quadrant != null)) {
      const button = action('', () => pick(point), 'day-point');
      button.dataset.n = point.quadrant;
      button.setAttribute('aria-label', names.get(point.task));
      button.title = names.get(point.task);
      const y = point.importance < importanceCutoff ? 100 - point.importance / importanceCutoff * 50 :
        (100 - point.importance) / (100 - importanceCutoff) * 50;
      button.style.left = `clamp(8px, ${urgentPosition(point.pressure)}%, calc(100% - 8px))`;
      button.style.top = `clamp(8px, ${y}%, calc(100% - 8px))`;
      plot.append(button);
    }
    const known = model.priorities.filter(item => item.quadrant != null);
    const unknown = model.priorities.filter(item => item.quadrant == null);
    if (known.length) pick(known[0]);
    else details.append(message('待补充数据的任务可在下方查看。'));
    const points = (items, empty) => items.length ? e('div', { class: 'day-point-list' },
      items.map(point => action(names.get(point.task), () => pick(point)))) : message(empty);
    const matrix = section('优先级四象限',
      e('div', { class: 'day-matrix-layout' }, e('span', { class: 'day-axis-y' }, '重要性 ↑'), plot,
        e('span', { class: 'day-axis-x' }, '紧迫度 →')),
      message(`重要性 ≥ ${importanceCutoff}，紧迫度 ≥ ${ratio(urgencyCutoff)}。紧迫度为剩余估算除以截止前的可用时间。`),
      details,
      e('details', {}, e('summary', {}, `已分类 · ${known.length} 项`), points(known, '暂无')),
      e('details', { open: !known.length && unknown.length > 0 },
        e('summary', {}, `待补充 · ${unknown.length} 项`), points(unknown, '暂无')));
    const strategy = section('剩余工作与战略时间',
      e('dl', { class: 'day-model-stats' },
        e('dt', {}, '手动剩余估算'), e('dd', {}, rounded(board.totals.remaining_estimate_minutes)),
        e('dt', {}, '当天剩余可用'), e('dd', {}, rounded(model.remaining_work_minutes)),
        e('dt', {}, '按历史 P50 校正'), e('dd', {}, model.calibrated_tasks
          ? `${rounded(model.calibrated_remaining_minutes)} · 覆盖 ${model.calibrated_tasks} 项` : '样本不足'),
        e('dt', {}, '重要且从容已排'), e('dd', {}, rounded(model.strategic_scheduled_minutes)),
        e('dt', {}, '战略时间目标'), e('dd', {}, rounded(model.strategic_target_minutes))),
      model.strategic_target_minutes > 0 ? meter(ui, model.strategic_scheduled_minutes / model.strategic_target_minutes, '战略时间预算') : null,
      model.calibrated_tasks ? message('有同类样本的任务按 P50 调整，其余保留手动估算。') : null,
      board.totals.unestimated_tasks ? message(`${board.totals.unestimated_tasks} 项任务未计入手动估算。`) : null,
      action('设置工作时段和预算', () => ui.edit(view, 'daily_plan', board.plan)));
    const estimates = Object.entries(model.estimates);
    const estimateTable = e('table', { class: 'day-estimates' },
      e('thead', {}, e('tr', {}, ['类别', '样本', 'P50', 'P80', 'P95'].map(title => e('th', { scope: 'col' }, title)))),
      e('tbody', {}, estimates.map(([category, sample]) => e('tr', {},
        e('th', { scope: 'row' }, category), e('td', {}, String(sample.count)),
        ['p50', 'p80', 'p95'].map(key => e('td', {}, ratio(sample[key])))))));
    const history = section('历史估时',
      message('按类别比较已完成任务的实际耗时 ÷ 初始估算。P50、P80、P95 至少需要 5、10、20 个样本。'),
      estimates.length ? e('div', { class: 'day-table-wrap' }, estimateTable)
        : message('暂无具有类别、初始估算和实际记录的已完成任务。'));
    const estimateRows = model.priorities.filter(point => point.p50_remaining_minutes != null).map(point => {
      const task = board.tasks.find(row => row.task.id === point.task);
      return ui.row(task.task.title, `手动 ${rounded(task.remaining_minutes)} · P50 ${rounded(point.p50_remaining_minutes)} · P80 ${rounded(point.p80_remaining_minutes)} · P95 ${rounded(point.p95_remaining_minutes)}`);
    });
    const revisions = board.estimate_revisions.map(item => ui.row(names.get(item.task),
      `${rounded(item.before)} → ${rounded(item.after)} · ${timeText(item.time, board.timezone, true).replace('T', ' ')} · ${item.reason || '未填写原因'}`));
    return e('div', { class: 'day-analysis-grid' }, matrix, e('div', { class: 'day-stack' }, strategy, history),
      section('任务耗时校正', ui.list(estimateRows, '样本达到要求后显示')),
      section('估时修订', ui.list(revisions, '当前看板任务暂无估时调整')));
  }

  window.PosDayBoard = { render, meter };
})();
