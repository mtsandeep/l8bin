import { useState, useEffect, useRef } from "react";

export default function Timer({ seconds = 15, onExpire }) {
  const [timeLeft, setTimeLeft] = useState(seconds);
  const intervalRef = useRef(null);
  const onExpireRef = useRef(onExpire);
  onExpireRef.current = onExpire;

  useEffect(() => {
    setTimeLeft(seconds);
    intervalRef.current = setInterval(() => {
      setTimeLeft((prev) => {
        if (prev <= 1) {
          if (intervalRef.current) {
            clearInterval(intervalRef.current);
            intervalRef.current = null;
          }
          // Use setTimeout to call onExpire after render cycle
          setTimeout(() => onExpireRef.current?.(), 0);
          return 0;
        }
        return prev - 1;
      });
    }, 1000);
    return () => {
      if (intervalRef.current) {
        clearInterval(intervalRef.current);
        intervalRef.current = null;
      }
    };
  }, [seconds]);

  const mins = Math.floor(timeLeft / 60);
  const secs = timeLeft % 60;
  const isUrgent = timeLeft <= 5;

  return (
    <div
      className={`flex items-center gap-2 font-mono font-bold border-[3px] border-retro-ink px-4 py-1.5 rounded-full shadow-neubrutalism-sm ${
        isUrgent
          ? "bg-retro-accent text-white animate-pulse"
          : "bg-white text-retro-ink"
      }`}
    >
      <i
        className={`fa-solid fa-stopwatch ${
          isUrgent ? "text-white" : "text-retro-accent"
        }`}
      ></i>
      <span>
        {String(mins).padStart(2, "0")}:{String(secs).padStart(2, "0")}
      </span>
    </div>
  );
}
