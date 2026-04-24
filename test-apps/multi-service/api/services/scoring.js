export function getPoints(guessed, correct, timeTaken = 0) {
  if (correct === 0) return guessed === 0 ? 1000 : 0;

  const diff = Math.abs(guessed - correct) / correct;

  let points = 0;
  if (diff === 0) points = 1000;
  else if (diff <= 0.05) points = 800;
  else if (diff <= 0.10) points = 600;
  else if (diff <= 0.20) points = 400;
  else if (diff <= 0.30) points = 200;
  else points = 0;

  // Speed bonus - only if user got some points (within 30% accuracy)
  if (points > 0) {
    if (timeTaken > 0 && timeTaken < 5) points += 500;
    else if (timeTaken > 0 && timeTaken < 10) points += 200;
  }

  return points;
}

export function checkLevelPass(answers, correctCalories) {
  if (!answers.length) return false;

  let totalDiff = 0;
  for (const answer of answers) {
    const correct = correctCalories[answer.foodId];
    if (correct && correct > 0) {
      const diff = Math.abs(answer.guessed - correct) / correct;
      // Cap individual question difference at 50% for average calculation
      totalDiff += Math.min(diff, 0.50);
    }
  }
  const avgDiff = totalDiff / answers.length;
  return avgDiff <= 0.30;
}

export function getTitle(totalScore) {
  if (totalScore >= 10000) return "Calorie God";
  if (totalScore >= 8000) return "Nutrition Ninja";
  if (totalScore >= 6000) return "Snack Analyst";
  if (totalScore >= 4000) return "Food Explorer";
  if (totalScore >= 2000) return "Hungry Guessr";
  if (totalScore >= 500) return "Calorie Rookie";
  return "Lost in the Kitchen";
}
