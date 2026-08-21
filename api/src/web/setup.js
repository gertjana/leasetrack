// Setup: keep the derived lease end date in sync with the start date and
// duration inputs.

function calcEnd() {
  const start = document.getElementById('lease_start').value;
  const years = parseInt(document.getElementById('lease_years').value, 10);
  const el = document.getElementById('end-date');
  if (start && years >= 1) {
    const d = new Date(start);
    d.setFullYear(d.getFullYear() + years);
    el.textContent = d.toISOString().slice(0, 10);
  }
}

calcEnd();
