/// Sum floats with CPython 3.12's Neumaier compensation.
pub(crate) fn py_sum(values: impl IntoIterator<Item = f64>) -> f64 {
    let (mut total, mut c): (f64, f64) = (0.0, 0.0);
    for x in values {
        let t = total + x;
        if total.abs() >= x.abs() {
            c += (total - t) + x;
        } else {
            c += (x - t) + total;
        }
        total = t;
    }
    if c != 0.0 && c.is_finite() {
        total += c;
    }
    total
}
