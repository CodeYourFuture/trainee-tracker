use sheets::types::CellData;

pub(crate) fn cell_string(cell: &CellData) -> Result<String, anyhow::Error> {
    let value = cell.effective_value.clone();
    if let Some(value) = value {
        Ok(value.string_value)
    } else {
        Ok(String::new())
    }
}
