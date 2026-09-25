-- fires
-- The same table twice under two names, as JOB's movie_link queries do.
SELECT MIN(t1.title) AS first, MIN(t2.title) AS linked FROM title AS t1, movie_link AS ml, title AS t2
WHERE t1.id = ml.movie_id AND ml.linked_movie_id = t2.id AND t1.production_year > 1990;
