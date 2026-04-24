import { useState, useCallback, useEffect } from "react";

export default function CalorieSlider({
  value,
  onChange,
  min = 0,
  max = 2000,
  defaultValue = 500,
}) {
  const [localValue, setLocalValue] = useState(value || defaultValue);

  // Sync local state with prop value when it changes
  useEffect(() => {
    setLocalValue(value || defaultValue);
  }, [value, defaultValue]);

  const handleChange = useCallback(
    (val) => {
      const num = Math.max(min, Math.min(max, parseInt(val) || 0));
      setLocalValue(num);
      onChange?.(num);
    },
    [onChange, min, max],
  );

  const percentage = ((localValue - min) / (max - min)) * 100;

  return (
    <div className="w-full max-w-2xl mx-auto space-y-6">
      {/* Calorie Display Box */}
      <div className="text-center">
        <div className="inline-block bg-retro-blue border-[3px] border-retro-ink rounded-2xl px-4 sm:px-6 py-3 sm:py-4 shadow-neubrutalism-lg w-full max-w-md">
          <div className="sm:flex items-center gap-2 sm:gap-4">
            <div className="font-mono text-xs sm:text-sm text-white/80 uppercase tracking-wider mb-1 sm:mb-0">
              I think
            </div>
            <input
              type="number"
              value={localValue}
              min={min}
              max={max}
              onChange={(e) => handleChange(e.target.value)}
              className="font-black text-2xl sm:text-5xl lg:text-6xl text-white bg-transparent border-none text-center flex-1 outline-none focus:ring-0"
            />
            <div className="font-mono text-xs sm:text-sm text-white/80 uppercase tracking-wider">
              Calories
            </div>
          </div>
        </div>
      </div>

      {/* Slider */}
      <div className="space-y-4 px-2">
        <div className="relative py-4">
          {/* Progress Track */}
          <div className="absolute top-1/2 left-0 right-0 h-3 bg-retro-gray border-[3px] border-retro-ink rounded-lg -translate-y-1/2 pointer-events-none">
            <div
              className="h-full bg-gradient-to-r from-green-400 via-orange-400 to-retro-accent rounded-l-md transition-all duration-150"
              style={{ width: `${percentage}%` }}
            />
          </div>

          <input
            type="range"
            min={min}
            max={max}
            step={10}
            value={localValue}
            onChange={(e) => handleChange(e.target.value)}
            className="w-full relative z-10"
          />
        </div>
      </div>
    </div>
  );
}
