export function FlexStationPicker({ value, snapshot, onChange }: {
  value: string; snapshot?: Record<string, unknown>; onChange: (station: string) => void;
}) {
  const stations = Array.isArray(snapshot?.available_stations)
    ? snapshot.available_stations.filter((name): name is string => typeof name === "string" && !!name) : [];
  const saved = value === "OM0RX-xLog" && !stations.includes(value) ? "" : value;
  const selected = typeof snapshot?.selected_station === "string" ? snapshot.selected_station : "";
  return <label>SmartSDR station<select value={saved} onChange={event => onChange(event.target.value)}>
    <option value="">Automatic{selected && !saved ? ` · ${selected}` : ""}</option>
    {[...new Set([...stations, ...(saved ? [saved] : [])])].map(name => <option key={name}>{name}</option>)}
  </select></label>;
}
