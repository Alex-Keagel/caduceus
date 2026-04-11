import { THEMES, type ThemeName } from "../theme";

interface ThemePickerProps {
  value: ThemeName;
  onChange: (theme: ThemeName) => void;
}

export default function ThemePicker({ value, onChange }: ThemePickerProps) {
  return (
    <label className="theme-picker">
      <span>Theme</span>
      <select value={value} onChange={(event) => onChange(event.target.value as ThemeName)}>
        {THEMES.map((theme) => (
          <option key={theme.id} value={theme.id}>
            {theme.label}
          </option>
        ))}
      </select>
    </label>
  );
}
