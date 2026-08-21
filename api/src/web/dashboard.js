// Dashboard: toggle the lease-info panel between its read-only view and the
// edit form, and keep the derived end date in sync with the inputs.

function toggleLeaseEdit() {
  const view = document.getElementById('cfg-view');
  const form = document.getElementById('cfg-form');
  const btn = document.getElementById('cfg-edit-btn');
  const editing = form.style.display !== 'none';
  view.style.display = editing ? '' : 'none';
  form.style.display = editing ? 'none' : '';
  btn.textContent = editing ? 'Edit' : 'Cancel';
}

function calcEnd() {
  const start = document.getElementById('cfg-start').value;
  const years = parseInt(document.getElementById('cfg-years').value, 10);
  const el = document.getElementById('cfg-end');
  if (start && years >= 1) {
    const d = new Date(start);
    d.setFullYear(d.getFullYear() + years);
    el.textContent = d.toISOString().slice(0, 10);
  }
}
