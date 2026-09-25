-- declines: not an equality of integer columns
-- A filter that reads two relations at once is not a semijoin.
SELECT MIN(t.title) AS movie_title FROM title AS t, movie_info AS mi WHERE t.id = mi.movie_id AND t.title < mi.info;
