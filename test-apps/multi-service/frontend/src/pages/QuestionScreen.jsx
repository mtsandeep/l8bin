import { useState, useEffect, useRef } from "react";
import Timer from "../components/Timer";
import CalorieSlider from "../components/CalorieSlider";

export default function QuestionScreen({ questions, level, onComplete }) {
  const [currentIndex, setCurrentIndex] = useState(0);
  const sliderConfig =
    level === 1
      ? { min: 0, max: 400, defaultValue: 100 }
      : level === 2
        ? { min: 0, max: 1000, defaultValue: 250 }
        : { min: 0, max: 3000, defaultValue: 500 };
  const [sliderValue, setSliderValue] = useState(sliderConfig.defaultValue);
  const sliderValueRef = useRef(sliderConfig.defaultValue);
  const [answers, setAnswers] = useState([]);
  const [timeLeft, setTimeLeft] = useState(15);
  const questionStartTimeRef = useRef(null);
  const timerIntervalRef = useRef(null);
  const advancedForIndexRef = useRef(-1);

  const currentQuestion = questions[currentIndex];
  const isLastQuestion = currentIndex === questions.length - 1;

  // Reset timer and slider on question change
  useEffect(() => {
    setSliderValue(sliderConfig.defaultValue);
    sliderValueRef.current = sliderConfig.defaultValue;
    setTimeLeft(15);
    questionStartTimeRef.current = Date.now();

    // Clear any existing timer
    if (timerIntervalRef.current) {
      clearInterval(timerIntervalRef.current);
      timerIntervalRef.current = null;
    }

    // Start new timer
    timerIntervalRef.current = setInterval(() => {
      setTimeLeft((prev) => {
        if (prev <= 1) {
          clearInterval(timerIntervalRef.current);
          timerIntervalRef.current = null;
          // Only advance if we haven't already advanced for this index
          if (advancedForIndexRef.current !== currentIndex) {
            advancedForIndexRef.current = currentIndex;
            const timeTaken = Math.max(
              0.1,
              (Date.now() - questionStartTimeRef.current) / 1000,
            );
            handleNextQuestion(sliderValueRef.current, timeTaken);
          }
          return 0;
        }
        return prev - 1;
      });
    }, 1000);

    return () => {
      if (timerIntervalRef.current) {
        clearInterval(timerIntervalRef.current);
        timerIntervalRef.current = null;
      }
    };
  }, [currentIndex]);

  const handleNextQuestion = (value, timeTaken) => {
    const answer = {
      foodId: currentQuestion?.foodId,
      guessed: value,
      timeTaken,
    };

    const newAnswers = [...answers, answer];
    setAnswers(newAnswers);

    if (isLastQuestion) {
      onComplete(newAnswers);
    } else {
      setCurrentIndex((prev) => prev + 1);
    }
  };

  const handleNext = () => {
    if (timerIntervalRef.current) {
      clearInterval(timerIntervalRef.current);
      timerIntervalRef.current = null;
    }
    const timeTaken = Math.max(
      0.1,
      (Date.now() - questionStartTimeRef.current) / 1000,
    );
    handleNextQuestion(sliderValueRef.current, timeTaken);
  };

  if (!currentQuestion) return null;

  const label = currentQuestion.category || currentQuestion.cuisine || "";
  const isUrgent = timeLeft <= 5;

  return (
    <div className="p-4 sm:p-4 lg:p-8 flex flex-col min-h-[calc(100vh-80px)]">
      <div className="max-w-4xl mx-auto w-full flex flex-col flex-grow">
        {/* Quiz Header */}
        <div className="flex items-center justify-between mb-6 sm:mb-8 animate-fade-in-up">
          <div className="inline-flex items-center gap-2 border-[3px] border-retro-ink bg-retro-blue text-white font-mono font-bold px-3 py-1 sm:px-4 sm:py-1.5 rounded-full text-xs sm:text-sm shadow-neubrutalism-sm">
            <i className="fa-solid fa-layer-group text-xs sm:text-sm"></i>
            <span className="text-xs sm:text-sm">
              QUESTION {currentIndex + 1} OF {questions.length}
            </span>
          </div>
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
              {String(Math.floor(timeLeft / 60)).padStart(2, "0")}:
              {String(timeLeft % 60).padStart(2, "0")}
            </span>
          </div>
        </div>

        {/* Progress Bar */}
        <div className="flex gap-1 sm:gap-2 mb-6 sm:mb-8">
          {questions.map((_, i) => (
            <div
              key={i}
              className={`h-2 sm:h-3 flex-1 border-[2px] border-retro-ink rounded-full relative overflow-hidden ${
                i < currentIndex ? "bg-retro-green" : "bg-retro-gray"
              }`}
            >
              {i === currentIndex && (
                <div
                  className="absolute inset-0 bg-gradient-to-r from-orange-400 to-retro-accent"
                  style={{
                    animation: "fillProgress 15s linear forwards",
                    transformOrigin: "left",
                  }}
                />
              )}
            </div>
          ))}
        </div>

        {/* Question Card */}
        <div
          className="bg-white border-[3px] border-retro-ink rounded-2xl p-4 sm:p-6 lg:p-8 shadow-neubrutalism-lg flex-grow flex flex-col animate-fade-in-up"
          style={{ animationDelay: "0.1s" }}
        >
          {/* Question Prompt */}
          <h2 className="text-xl sm:text-2xl lg:text-4xl font-black font-sans leading-tight mb-3 sm:mb-4 text-center">
            How many calories are in{" "}
            <span className="text-retro-accent underline decoration-[2px] sm:decoration-[4px] underline-offset-2 sm:underline-offset-4">
              {currentQuestion.name}
            </span>
            ?
          </h2>

          {/* Category / Cuisine + Serving Size */}
          <div className="flex justify-center gap-2 sm:gap-3 mb-4 sm:mb-6 flex-wrap">
            {label && (
              <span className="inline-flex items-center gap-1 border-[2px] border-retro-ink bg-retro-gray px-2 py-1 sm:px-3 sm:py-1 rounded-full font-mono text-xs sm:text-sm font-bold capitalize">
                <i
                  className={`fa-solid ${currentQuestion.category === "fruit" ? "fa-apple-whole" : currentQuestion.category === "vegetable" ? "fa-carrot" : currentQuestion.category === "meat" ? "fa-drumstick-bite" : currentQuestion.category === "fish" ? "fa-fish" : "fa-globe"} text-xs sm:text-sm`}
                ></i>
                {label}
              </span>
            )}
            <span className="inline-flex items-center gap-1 border-[2px] border-retro-ink bg-retro-blue text-white px-2 py-1 sm:px-3 sm:py-1 rounded-full font-mono text-xs sm:text-sm font-bold">
              <i className="fa-solid fa-weight-scale text-xs sm:text-sm"></i>
              {currentQuestion.servingSize}g
            </span>
          </div>

          {/* Top Ingredients - only for levels 2-3 */}
          {level > 1 &&
            currentQuestion.topIngredients &&
            currentQuestion.topIngredients.length > 0 && (
              <div className="mb-6 sm:mb-8">
                <div className="bg-retro-gray border-[2px] border-retro-ink rounded-xl p-3 sm:p-4">
                  <div className="text-xs sm:text-sm font-mono font-bold text-retro-muted mb-2 sm:mb-3 uppercase tracking-wide">
                    <i className="fa-solid fa-list-ol mr-1 sm:mr-2"></i>
                    Main Ingredients
                  </div>
                  <div className="grid grid-cols-1 sm:grid-cols-2 gap-1.5 sm:gap-2">
                    {currentQuestion.topIngredients
                      .slice(0, 5)
                      .map((ingredient, idx) => (
                        <div
                          key={idx}
                          className="flex items-center justify-between bg-white border-[2px] border-retro-ink rounded-lg px-2 py-1 sm:px-3 sm:py-1.5"
                        >
                          <span className="text-xs sm:text-sm font-bold text-retro-ink truncate">
                            {ingredient.name}
                          </span>
                          <span className="text-xs sm:text-sm font-mono font-bold text-retro-accent ml-2">
                            {ingredient.weight}g
                          </span>
                        </div>
                      ))}
                  </div>
                </div>
              </div>
            )}

          {/* Slider */}
          <div className="flex-grow flex flex-col justify-center">
            <CalorieSlider
              value={sliderValue}
              onChange={(value) => {
                setSliderValue(value);
                sliderValueRef.current = value;
              }}
              {...sliderConfig}
            />
          </div>

          {/* Next Button */}
          <div className="mt-0 sm:mt-10 pt-4 sm:pt-8 border-t-[3px] border-retro-ink border-dashed flex justify-center">
            <button
              onClick={handleNext}
              className="w-full sm:w-auto bg-retro-accent hover:bg-[#e03a2e] text-white border-[3px] border-retro-ink rounded-xl py-3 sm:py-4 px-6 sm:px-10 font-mono font-black text-lg sm:text-xl uppercase tracking-wider shadow-neubrutalism-lg transition-all active:translate-y-1 active:shadow-none flex items-center justify-center gap-2 sm:gap-3"
            >
              {isLastQuestion ? "Submit" : "Next Question"}{" "}
              <i className="fa-solid fa-arrow-right"></i>
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
